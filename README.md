# marcusdunnca

AWS account infrastructure, managed with OpenTofu and applied by GitHub Actions.

No long-lived AWS credentials exist anywhere in this system. CI authenticates
with short-lived OIDC tokens minted per job; the only human credentials are
IAM Identity Center sessions.

## Layout

| Path | Applied by | Holds |
| --- | --- | --- |
| `bootstrap/` | **a human, from a workstation** | State bucket, GitHub OIDC provider, the two CI roles, the CI permissions boundary |
| `infra/` | **CI, on push to `main`** | Account security baseline: CloudTrail, Access Analyzer, account-wide S3 public access block, EBS default encryption, password policy |
| `.github/workflows/` | — | plan on PR, apply on main, Dependabot auto-merge, lock refresh |
| `scripts/` | — | One-shot bootstrap and GitHub configuration |

`bootstrap/` is separate because it creates the very credentials CI runs as.
CI can plan it (to catch drift) but can never apply it.

## The trust chain

```
PR opened          →  OIDC sub = repo:MarcusDunn/marcusdunnca:pull_request
                   →  assumes marcusdunnca-gha-plan   (ReadOnlyAccess)

push to main       →  job declares `environment: production`
                   →  OIDC sub = repo:MarcusDunn/marcusdunnca:environment:production
                   →  assumes marcusdunnca-gha-apply  (Admin + guardrails)
```

The apply role's trust policy accepts *only* the environment subject. The
`production` environment is restricted to `main`. So apply is unreachable from
any branch, any fork, and any PR — enforced by AWS, not just by GitHub.

### What the apply role cannot do

`AdministratorAccess` is attached, then fenced in by an inline deny policy.
Explicit deny always wins, so these hold no matter how AWS widens the managed
policy later:

- Modify its own role, the plan role, the permissions boundary, or the OIDC
  provider — **CI cannot grant itself more power**
- Create IAM users, access keys, or console logins — **no long-lived credentials**
- Create any role without attaching the CI permissions boundary
- Stop or delete CloudTrail — **it cannot erase its own audit trail**
- Reconfigure the state bucket or delete state object versions
- Touch Identity Center, close the account, or leave an organization

## First-time setup

Target account: **812642122818** (`ca-central-1`).

The `marcusdunnca` AWS profile is declared in `nix-config/modules/aws.nix`, not
here — apply your home-manager config first so the profile exists.

```bash
nix develop                     # opentofu, awscli2, gh, jq; sets AWS_PROFILE
aws sso login

./scripts/bootstrap.sh          # creates state bucket, OIDC provider, CI roles
./scripts/github-setup.sh       # repo settings, ruleset, environment, role ARNs
```

`bootstrap.sh` asserts the authenticated account is 812642122818 and refuses to
run otherwise — this machine has a dozen work profiles configured, and an
admin-level IAM apply against the wrong one is not a recoverable mistake.

`bootstrap.sh` handles the chicken-and-egg problem: the first apply runs against
local state because the bucket it creates does not exist yet, then migrates that
state into the bucket and deletes the local copy.

## Day-to-day

```bash
nix develop
cd infra
tofu init -backend-config=backend.hcl
tofu plan                       # against real state; requires an SSO session
```

Then open a PR. Merging to `main` applies. Do not apply from your workstation —
the SSO admin role can, but the CI role is the intended path and drift between
the two is how state gets confusing.

## Branch protection

This repository is **public**. GitHub does not offer rulesets on private
repositories below the Pro plan, and enforced branch protection was judged
worth more than keeping the topology unpublished. Nothing here is a
credential — CI authenticates with per-job OIDC tokens — so what is exposed is
reconnaissance value (account ID, role and bucket names), not access.

Two consequences follow from being public, both handled:

- **Fork PRs cannot reach the plan role.** The OIDC subject for a PR is
  `repo:MarcusDunn/marcusdunnca:pull_request` no matter which fork the branch
  came from. GitHub withholds `id-token: write` from fork PRs, but the plan job
  additionally refuses to run unless the PR head is this repository.
- **Secret scanning with push protection is enabled**, so a recognised
  credential is rejected at push time rather than after it is public.
- **Workflow runs from every external contributor require approval**, not just
  first-time ones, so no stranger's push causes CI to execute unreviewed.
- **Only an allowlist of actions may run**, and GitHub itself rejects any
  workflow referencing an action by tag instead of a commit SHA.

The ruleset on `main` has **no bypass actors**, including you:

- All changes via pull request (0 required approvals — solo repo, and GitHub
  forbids self-approval, so requiring one would force routine bypasses and
  train the habit of ignoring the rules)
- Signed commits required
- Linear history, squash merges only
- Force-push and deletion blocked
- `plan (bootstrap)` and `plan (infra)` must pass, against current `main`

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

The lock-refresh PRs are merged by hand on purpose. They are authored with
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
