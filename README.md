# marcusdunnca

AWS account infrastructure, managed with OpenTofu and applied by GitHub Actions.

No long-lived AWS credentials exist anywhere in this system. CI authenticates
with short-lived OIDC tokens minted per job; the only human credentials are
IAM Identity Center sessions.

## Threat model

**Assume the CI pipeline is compromised** — an attacker has write access to this
repository, and can therefore modify workflows, open and merge pull requests,
and reach both CI roles.

The goal is not that nothing happens. It is that the blast radius is small,
bounded, and fully recorded. Concretely, an attacker who owns this pipeline
still cannot:

- escalate privileges — the apply role holds **no IAM role or policy write
  permission at all**, so there is no first step
- mint anything that outlives the job — no IAM users, access keys, or console
  logins
- run up a bill — every region but `ca-central-1`/`us-east-1` is denied, and
  expensive service families are denied by name on top of not being allowlisted
- destroy or ransom the record — state object versions cannot be deleted, and
  CloudTrail cannot be stopped, deleted, or retargeted
- weaken the account's defences — the security floor is human-applied and CI is
  denied permission to touch it
- read application data — the plan role's object reads are confined to the state
  bucket

## Layout

| Path | Applied by | Holds |
| --- | --- | --- |
| `bootstrap/` | **a human, from a workstation** | State bucket, GitHub OIDC provider, the two CI roles, the guardrail policy, **and the account security floor** |
| `infra/` | **CI, on push to `main`** | Operational resources CI is trusted with; where application infrastructure will go |
| `scripts/` | — | One-shot bootstrap and GitHub configuration |

The security floor — CloudTrail, the account-wide S3 public access block, the
password policy, EBS encryption defaults, alternate contacts — lives in
`bootstrap/`, not `infra/`. Under the threat model above, the controls that
would let an attacker weaken the account or conceal their activity must not be
writable by the thing being attacked.

## The trust chain

```
PR opened      →  sub = repo:MarcusDunn@51931484/marcusdunnca@1350868756:pull_request
               →  assumes marcusdunnca-gha-plan   (enumerated read-only)

push to main   →  job declares `environment: production`
               →  sub = repo:MarcusDunn@51931484/marcusdunnca@1350868756:environment:production
               →  assumes marcusdunnca-gha-apply  (enumerated write)
```

Subjects are in GitHub's **immutable form**, with the owner and repository IDs
embedded, pinned via the `actions/oidc/customization/sub` API. Binding trust to
numeric IDs rather than names means renaming this repository — or a third party
later claiming the freed-up name `MarcusDunn/marcusdunnca` — cannot produce a
token this account will accept.

The apply role's trust policy accepts *only* the environment subject, and the
`production` environment is restricted to `main`. So "apply only from main" is
enforced by AWS, not merely by GitHub.

### Expanding scope

Both roles are enumerated allowlists — neither uses an AWS managed policy.
`AdministratorAccess` and `ReadOnlyAccess` are both far wider than this account
needs, and AWS can widen them further without notice.

When the application needs a new service, add the specific actions to
`bootstrap/iam.tf` in a pull request. That review *is* the control. Note the
guardrail policy is pure `Deny` and attached to both roles, so anything it
refuses stays refused no matter what is added to an allowlist.

If `iam:CreateRole` is ever added, the permissions boundary in `bootstrap/iam.tf`
must be wired to it at the same time — otherwise that one addition converts the
apply role into a privilege-escalation path.

## Branch protection

This repository is **public**. GitHub does not offer rulesets on private
repositories below the Pro plan, and enforced branch protection was judged worth
more than keeping the topology unpublished. Nothing here is a credential, so
what is exposed is reconnaissance value, not access.

The ruleset on `main` has **no bypass actors**, including you:

- all changes via pull request (0 required approvals — solo repo, and GitHub
  forbids self-approval, so requiring one would force routine bypasses and train
  the habit of ignoring the rules)
- signed commits required
- linear history, squash merges only
- force-push and deletion blocked
- `plan (bootstrap)` and `plan (infra)` must pass, against current `main`

Alongside it: secret scanning with push protection; workflow runs require
approval from **all** external contributors; only an allowlist of actions may
run; and GitHub rejects any workflow referencing an action by tag rather than a
commit SHA.

### A note on the fork guard

`tofu-plan.yml` refuses to run when the PR head is not this repository. This is
**defence in depth, not a boundary**: for `pull_request` events GitHub runs the
workflow file from the PR head, so a fork author can simply delete that check in
their own copy.

What actually holds is that GitHub withholds `id-token: write` from fork PRs — a
workflow's `permissions:` block cannot elevate it — plus the external-contributor
approval requirement. And should both fail, the plan role is read-only and
cannot read application data.

## First-time setup

Target account: **812642122818** (`ca-central-1`).

The `marcusdunnca` AWS profile is declared in `nix-config/modules/aws.nix`, not
here — apply your home-manager config first so the profile exists.

```bash
nix develop                     # opentofu, awscli2, gh, jq; sets AWS_PROFILE
aws sso login

./scripts/bootstrap.sh          # state bucket, OIDC provider, CI roles, security floor
./scripts/github-setup.sh       # repo settings, ruleset, environment, role ARNs
```

`bootstrap.sh` asserts the authenticated account is 812642122818 and refuses to
run otherwise — this machine has a dozen work profiles configured, and an
admin-level IAM apply against the wrong one is not a recoverable mistake.

## Day-to-day

```bash
nix develop
cd infra
tofu init -backend-config=backend.hcl
tofu plan
```

Then open a PR. Merging to `main` applies. Changes to `bootstrap/` are planned by
CI but never applied by it — run `./scripts/bootstrap.sh` yourself.

## Dependency updates

Everything is version-pinned: Actions to commit SHAs, providers to versions and
hashes in `.terraform.lock.hcl`, nixpkgs in `flake.lock`.

| What | How | Merge |
| --- | --- | --- |
| GitHub Actions SHAs | Dependabot, 14-day cooldown | auto-merged if non-major |
| Provider constraints | Dependabot, 14-day cooldown | auto-merged if non-major |
| `.terraform.lock.hcl`, `flake.lock` | `update-locks.yml`, weekly | manual |

The 14-day cooldown means a release must survive two weeks in public before it
is proposed here — long enough for a malicious or broken publish to be caught.

**Auto-merge is gated on the plan being empty.** Merging to `main` applies
immediately, so a Dependabot PR that would actually change AWS resources fails
its check and waits for you. A no-op version bump merges itself.

Lock-refresh PRs are merged by hand on purpose. They are authored with
`GITHUB_TOKEN`, and GitHub does not trigger workflow runs for such PRs, so their
checks do not start automatically. Fixing that would need either a long-lived
token or a ruleset bypass actor; neither is worth it for a lockfile bump. Close
and reopen the PR to run its checks.

## Cost

The baseline is free-tier by design, given the 1-cent budget alert:

- CloudTrail — first management-events trail per account is free
- IAM Access Analyzer, account public access block, EBS encryption default,
  password policy — free
- S3 — pennies, bounded by lifecycle rules (state versions expire at 90 days,
  CloudTrail logs at 400)

GuardDuty, AWS Config, and Security Hub are deliberately **not** enabled.
Set `state_bucket_use_cmk = true` in `bootstrap/` to encrypt state with a
customer-managed KMS key (~$1/month) once state holds anything sensitive.
