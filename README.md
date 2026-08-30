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
  CloudTrail cannot be stopped, deleted, or retargeted. The state bucket also has
  S3 Object Lock in GOVERNANCE mode (7 days), so version immutability is a
  property of the bucket rather than resting solely on an identity policy;
  `s3:BypassGovernanceRetention` is denied to both roles
- poison the human's apply — CI cannot write `bootstrap/terraform.tfstate`.
  OpenTofu resolves providers from *state*, not only config, so a writable
  bootstrap state would mean arbitrary code execution on the operator's
  workstation under `AdministratorAccess` at the next `bootstrap.sh` run
- spend quietly — a budget and an immediate cost-anomaly subscription are managed
  here as code, and CI is denied the ability to modify or delete either
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

### Application roles

CI **can** create IAM roles, but only ones carrying the permissions boundary.
That boundary is an allowlist — `lambda`, `s3`, `sqs`, `sns`, `logs`,
`cloudfront`, `dynamodb`, plus X-Ray write — so a created role cannot act outside those
services *even if a policy granting `Action: "*"` is attached to it*. Verified:
under the boundary, `dynamodb:PutItem`, `ses:SendEmail` and `kms:Decrypt` are
denied while `lambda:InvokeFunction` and `sqs:SendMessage` are allowed.

Two conditioned denies make this safe:

- `DenyRoleWorkWithoutBoundary` — `iam:CreateRole` without the boundary is
  refused. A missing `iam:PermissionsBoundary` key makes `StringNotEquals` true,
  so no-boundary fails closed.
- `DenyPassRoleExceptToAppServices` — `iam:PassRole` is confined to
  `lambda.amazonaws.com` and `edgelambda.amazonaws.com`. Unconstrained PassRole
  is an escalation primitive; constrained it is ordinary wiring.

Widening `app_service_actions` widens every application role at once, so it
deserves the same scrutiny as widening the apply role itself. Its read-only
counterpart `app_service_read_actions` is granted to the plan role so `tofu
plan` can refresh application resources — keep the two in step, or the first PR
touching a new service fails its plan.

The read list deliberately excludes anything returning *contents*: no
`s3:GetObject` outside the state bucket, no `dynamodb:GetItem`/`Scan`, no
`sqs:ReceiveMessage`. The plan role is reachable from any pull request, so it
must be able to see configuration without seeing data.

Note CloudFront create/update was removed from the expensive-service denies to
allow this. CloudFront is global, so the region lock gives it no cost
containment and data-transfer-out is unbounded — the $1 budget and the $1
anomaly subscription are what bound it now.

### Secrets

SSM Parameter Store, split into two namespaces that differ in who can read them:

| Prefix | Managed by | Readable by |
| --- | --- | --- |
| `/marcusdunnca/secret/*` | **nobody — created out of band** | runtime roles only |
| `/marcusdunnca/config/*` | Terraform | runtime roles, apply, plan |

Secrets are created with `aws ssm put-parameter` and are **never referenced by
value in Terraform** — only by name, in a Lambda environment variable and an IAM
policy resource ARN. Neither CI role can read them:

```bash
aws ssm put-parameter --name /marcusdunnca/secret/jwt-signing-key \
  --type SecureString --value "$(openssl rand -base64 48)" --overwrite
```

This is not squeamishness about state. The plan role is reachable from **any
pull request** on a public repository, and it can read state objects — so any
secret that reaches state is a secret any stranger's PR can print. Rotation is
the same command with a fresh value.

`SecureString` with the AWS-managed `aws/ssm` key is free. Secrets Manager is
$0.40/secret/month, and a customer-managed KMS key is ~$1/month — neither buys
anything here.

The provider does support write-only arguments (`aws_ssm_parameter.value_wo`,
verified present in aws 6.62.0) which keep a value out of state. They do not
help for CI-applied resources, because the applier still needs the value —
which for `infra/` would mean putting it in a GitHub secret.

## Branch protection

This repository is **public**. GitHub does not offer rulesets on private
repositories below the Pro plan, and enforced branch protection was judged worth
more than keeping the topology unpublished. Nothing here is a credential, so
what is exposed is reconnaissance value, not access.

### Repository write access grants the apply role. This is accepted.

There is **no human gate** between merging to `main` and `tofu apply`. That is a
deliberate position, not an oversight, and the GitHub-side rules below should be
read as hygiene rather than as a security boundary.

A required reviewer on the `production` environment was tried and removed. It
did not work: approving a deployment is an API call authorized by the `repo`
scope, and the token holding that scope lives on the same machine as the
pipeline. An attacker who owns the pipeline reads the token and approves their
own deployment. It cost five minutes per deploy and bought false confidence.

More generally, everything on the GitHub side is reachable by someone with write
access. They can open and merge their own PR (0 required approvals). Because
`pull_request` runs the workflow file **from the PR head**, they can redefine the
checks meant to gate their own change — and `integration_id` pinning does not
help, because the `github-actions` app is what runs every workflow here,
including a hostile one named `plan (bootstrap)`.

**The containment is in AWS, not in GitHub.** See the threat model above: the
apply role cannot escalate, cannot mint durable credentials, cannot leave two
regions, cannot silence the audit trail or the spend alarms, and cannot touch
the security floor. An attacker who owns this pipeline gets to change the
account alias and the Access Analyzer, and everything they do is recorded.

A genuine gate would require an approver identity the pipeline host cannot
reach — a separate account whose credentials exist only on a phone, with
`prevent_self_review: true`. Any approval path terminating in a token on the CI
host is theatre by construction.

The ruleset on `main` has **no bypass actors**, including you:

- all changes via pull request (0 required approvals — solo repo, and GitHub
  forbids self-approval, so requiring one would force routine bypasses and train
  the habit of ignoring the rules)
- signed commits required
- linear history, squash merges only
- force-push and deletion blocked
- `plan (bootstrap)` and `plan (infra)` must pass, against current `main`, and
  must be reported by the `github-actions` app (`integration_id` pinned, which
  stops Statuses-API forgery but *not* a hostile workflow in the PR itself)

Alongside it: secret scanning with push protection; Dependabot alerts and
security updates; workflow runs require approval from **all** external
contributors; only four named action repositories may run; and GitHub
rejects any workflow referencing an action by tag rather than a commit SHA.

Exact-SHA allowlisting was tried and reverted: every Dependabot SHA bump made
its own PR unmergeable, and with Dependabot security updates enabled that meant
the emergency path was the one that jammed.

### Two things that are weaker than they look

**The fork guard is defence in depth, not a boundary.** For `pull_request`
events GitHub runs the workflow file from the PR head, so a fork author can
delete the check in their own copy. What actually holds is that GitHub withholds
`id-token: write` from fork PRs — a `permissions:` block cannot elevate it — plus
the external-contributor approval requirement. And if both failed, the plan role
is read-only and cannot read application data.

Note the guard is a failing **step**, never a job-level `if:`. GitHub reports a
conditionally-skipped *job* as **Success** to required status checks, so a
job-level `if:` on a required check is a free pass rather than a gate. This
shipped as a live vulnerability once; do not reintroduce it.

**`required_signatures` attests to origin, not authorship.** GitHub signs any
commit created through its API or web editor, so an attacker with write access
gets Verified commits without holding a key. And with squash-only merges GitHub
authors the merge commit itself. The rule is kept — it costs nothing and stops
naive direct pushes — but do not build an argument on it. Commit authenticity
comes from the environment reviewer above.

### Controls that can fail silently

Three assertions run on every PR via the `health` job in `tofu-plan.yml`, all
hard failures: CloudTrail is actually delivering, the cost-alert topic has at
least one confirmed subscriber, and the root account is hardened (MFA present,
no root access keys, no IAM users).

Root is checked because it bypasses everything else here — the guardrails, the
permissions boundary, Object Lock, CloudTrail. Nothing else in this repo would
notice if root MFA were removed.

The first two exist because they have already failed silently:

- **CloudTrail delivered nothing for 108 minutes** while reporting
  `IsLogging: true`. A bucket-policy statement copied from the state bucket
  denied CloudTrail's own writes, because `aws:PrincipalAccount` is unset for
  service principals and `StringNotEquals` against a missing key is true. Only
  `LatestDeliveryError` showed it.
- **The cost-alert SNS topic can sit at zero confirmed subscribers** while
  `tofu apply` reports success. Email subscriptions need a human to click a
  link, and any edit to `cost_alert_emails` recreates them — silently reverting
  alerting to dead. OpenTofu cannot detect this: a pending subscription's ARN is
  the literal string `PendingConfirmation`, so there is nothing to diff.

Check both by hand with:

```bash
aws cloudtrail get-trail-status --name marcusdunnca-management-events \
  --query '{Logging:IsLogging,Err:LatestDeliveryError}'
aws sns get-topic-attributes --region us-east-1 \
  --topic-arn arn:aws:sns:us-east-1:812642122818:marcusdunnca-cost-alerts \
  --query 'Attributes.{Confirmed:SubscriptionsConfirmed,Pending:SubscriptionsPending}'
```

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
| `.terraform.lock.hcl`, `flake.lock` |  Dependabot `nix` ecosystem, 14-day cooldown | manual |

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

Near zero, but **not exactly zero**, and the difference is worth knowing.

Free: the first CloudTrail management-events trail, IAM Access Analyzer, the
account public access block, EBS encryption defaults, the password policy,
S3 Object Lock, Cost Explorer anomaly detection, and the first two AWS Budgets
(two exist — this repo's and the console-created zero-spend one).

Not free:

- **CloudTrail S3 data events** — billed per event ($0.10 per 100,000), not
  covered by the free management-events trail. Scoped to the state bucket only,
  which sees a handful of object operations per CI run, so this is fractions of
  a cent per month. It was briefly scoped to the CloudTrail bucket as well,
  which created a feedback loop: log delivery is itself a data event, so
  CloudTrail logged its own writes and generated more of them. Do not put the
  trail bucket back in that selector.
- **S3 storage and requests** — a few hundred KB across both buckets, bounded by
  lifecycle rules (state versions expire at 90 days, logs at 400).
- **SNS email** — first 1,000 notifications per month are free; alerts here are
  rare by construction.

The managed budget is **$1/month**, alerting at 80% actual, 100% actual, and on
a forecast breach. That is deliberately just above the real floor: a genuinely
zero-spend threshold false-alarms every month on a few cents of S3 and data
events, which trains you to ignore it.

### Spend controls

Three layers, because Bedrock and CloudFront are metered and the region lock
does not bound either:

1. **Model scoping.** There is no IAM condition key for token count, so *which
   model* is the only lever IAM offers. `bedrock:InvokeModel` is scoped to the
   Sonnet and Haiku families; Opus is `implicitDeny`, for both the apply role
   and any boundary-capped runtime role. Widen `bedrock_allowed_model_families`
   deliberately, knowing the price difference.
2. **Alerting.** $10 budget at 80/100% actual and 100% forecast, plus a Cost
   Anomaly subscription at $1 that publishes immediately via SNS.
3. **An automated circuit breaker.** At 90% of budget, AWS Budgets itself
   attaches `marcusdunnca-spend-brake` to both CI roles, denying Bedrock and
   further resource creation. No human, no Lambda, no dependency on anything in
   this repo still working. Free — the first two action-enabled budgets cost
   nothing.

The brake is deliberately one-way: clearing it needs a human with Identity
Center admin, because CI cannot modify its own role. That is the property you
want at 3am during a runaway.

Realistically pennies per month while idle. GuardDuty, AWS Config, and Security
Hub are deliberately **not** enabled. Set `state_bucket_use_cmk = true` in
`bootstrap/` to encrypt state with a customer-managed KMS key (~$1/month) once
state holds anything sensitive.
