# Security audit — 2026-09-05

Whole-repository review at commit `7f36ac5`: the two Rust Lambdas and the
shared crate under `app/`, the SPA under `web/`, both OpenTofu root modules,
every workflow, and both scripts. Every file was read; nothing was sampled.

Also run, from this checkout:

| Check | Result |
| --- | --- |
| `cargo audit` against `app/Cargo.lock` | see §Dependencies |
| `pnpm audit` (prod and dev) against `web/pnpm-lock.yaml` | no known vulnerabilities |
| `cargo test --workspace` | see §Tests |
| Git history scan for credentials, key material, state files | nothing found; only `infra/credentials.auto.tfvars` (public keys, by design) and `web/.env.example` |
| Search of the SPA for HTML/JS sinks (`dangerouslySetInnerHTML`, `innerHTML`, `eval`, `srcdoc`, …) | none |

**Not verified, and the report says so where it matters:** this session has no
AWS credentials, so nothing below was confirmed with `tofu plan`, the IAM policy
simulator, or CloudTrail. The AWS documentation site is blocked from this
session's network, so one condition-key detail in H-2 is stated from memory
and flagged. Treat every IAM finding as "the policy as written permits this",
which is what the repository controls.

## Summary

The application layer is unusually well defended for its size: exact-origin
WebAuthn with user verification required, single-use challenges consumed by
`DeleteItem … ALL_OLD` before any cryptography runs, an HS256-pinned JWT whose
key never leaves SSM, presigned uploads that pin key, content type *and*
content length, and an answer key that is unrepresentable in the quiz payload
by type rather than by discipline. No injection, no auth bypass, no leak of the
key was found.

The material findings are in `bootstrap/iam.tf`, and they undercut three
claims the README makes about a compromised pipeline. The apply role has since
been granted IAM role management (correctly, with a boundary), but two things
were not carried along with that expansion:

- the roles and policies that *bootstrap* owns but that are not in the
  control-plane deny list — the budget circuit breaker and its executor — are
  writable by CI, and
- the deny that was written to stop a boundary-wearing principal taking over
  an unbounded role (`role_takeover_actions`) was never attached to anything.

Together those are a privilege-escalation path and a way to switch off the
spend brake. Both are a small, local change to fix.

| ID | Severity | Finding |
| --- | --- | --- |
| H-1 | High | The apply role can rewrite, delete or re-trust the spend brake and its executor role. The "one-way circuit breaker" is not one-way. |
| H-2 | High | Trust-policy rewrite plus `PassRole` to Lambda lets the apply role run code as any boundary-less role in the account. `role_takeover_actions` is dead code. |
| M-1 | Medium | Application data (the DynamoDB table and its PITR backups, the documents bucket) is not protected from a compromised apply role; the boundary denies these deletes, the guardrail does not. |
| M-2 | Medium | The two unauthenticated auth routes write to a 5 WCU table on every call. At the gateway's own rate limit that exhausts write capacity and pollutes the scans every list screen depends on. |
| M-3 | Medium | Dependabot auto-merge covers every ecosystem except `github_actions`, so a Cargo or npm security bump is merged and deployed to production with no human reading it. The README says lock bumps are manual. |
| M-4 | Medium | No security response headers on the site. With the session token in `localStorage`, a CSP is the one control that would bound an XSS. |
| L-1 | Low | `pnpm@latest` is fetched unpinned inside the job that holds the apply role. |
| L-2 | Low | The registration token lands in Lambda configuration and in state, both readable by the plan role from any pull request, and stays in state history for 90 days. |
| L-3 | Low | Three README/comment claims no longer hold and should be corrected so the threat model reads true. |
| L-4 | Low | The CloudTrail bucket has no Object Lock; the documents bucket has no data-event logging. |
| L-5 | Low | Policy hygiene: an unconditioned `iam:CreateServiceLinkedRole` makes the conditioned one dead; three budget-action mutations are missing from the deny list; one dead local. |
| I-1 | Info | The HTTP API's `execute-api` endpoint is directly reachable and the stage throttle is aggregate, so any client can starve the owner. Accepted-cost items, recorded for completeness. |

## High

### H-1 — The spend brake is defeatable by the apply role

**Where.** `bootstrap/budget_action.tf:56-60` (the `spend_brake` policy),
`:82-86` (the `budgets_action` role), `:91-108` (its permissions);
`bootstrap/iam.tf:47-53` (`ci_control_plane_arns`), `:1010-1044`
(`ManageApplicationRoles`, `resources = ["*"]`).

**What.** `ManageApplicationRoles` grants the apply role `iam:CreatePolicyVersion`,
`iam:DeletePolicy`, `iam:DeleteRole`, `iam:UpdateAssumeRolePolicy` and
`iam:PassRole` on `*`. The only thing that narrows those is
`DenyTamperingWithCIControlPlane` (`iam.tf:514-519`), whose resource list is
the two CI roles, the boundary, the guardrail and the OIDC provider. The
spend-brake policy and the `budgets-action` role are not in it. So a
compromised apply role can, before or after the brake fires:

1. `iam:CreatePolicyVersion --set-as-default` on
   `marcusdunnca-spend-brake` with a body that denies nothing. The budget action
   then "succeeds" and attaches a policy that does nothing. The apply role
   cannot detach the brake from itself (`iam:Detach*` on its own ARN is
   denied), but it never needs to.
2. `iam:DeletePolicy` on the brake while it is unattached, so the action fails
   to attach anything.
3. `iam:DeleteRole` or `iam:UpdateAssumeRolePolicy` on
   `marcusdunnca-budgets-action`, so AWS Budgets can no longer assume the
   executor.

`DenyRoleWorkWithoutBoundary` (`iam.tf:574-590`) does block `PutRolePolicy` and
`AttachRolePolicy` on the executor, because it carries no boundary; it does not
cover `UpdateAssumeRolePolicy`, `DeleteRole`, or any policy-version action.

**Why it matters.** The README (`README.md:375`) and the file's own header
(`budget_action.tf:15`) describe the brake as "deliberately one-way: clearing
it needs a human with Identity Center admin". That is the property the whole
denial-of-wallet story rests on, and it does not hold against the threat model
the repository states — a pipeline with write access. Everything an attacker
does here is recorded by CloudTrail, but the point of the brake is to act
before anyone reads a log.

**Fix.** Add the executor role and the brake policy to the control-plane deny
list, and constrain the executor so it can only ever attach the one policy it
exists for:

```hcl
# bootstrap/iam.tf — locals
ci_control_plane_arns = [
  local.plan_role_arn,
  local.apply_role_arn,
  local.boundary_arn,
  local.guardrail_arn,
  local.oidc_arn,
  # The spend circuit breaker and the role AWS Budgets assumes to apply it.
  # Both are bootstrap-owned and boundary-less; CI must not be able to rewrite,
  # delete, or re-trust either, or the brake stops being one-way.
  "${local.iam_root}:role/${var.project}-budgets-action",
  "${local.iam_root}:policy/${var.project}-spend-brake",
]
```

```hcl
# bootstrap/budget_action.tf — data.aws_iam_policy_document.budgets_action
statement {
  sid       = "AttachAndDetachTheBrake"
  effect    = "Allow"
  actions   = ["iam:AttachRolePolicy", "iam:DetachRolePolicy"]
  resources = [local.plan_role_arn, local.apply_role_arn]

  # Listing the policy ARN as a resource does nothing for Attach/Detach — the
  # resource is the role. This is the condition that actually limits WHICH
  # policy can be attached, so a hijacked executor cannot attach
  # AdministratorAccess to the CI roles.
  condition {
    test     = "ArnEquals"
    variable = "iam:PolicyARN"
    values   = [aws_iam_policy.spend_brake.arn]
  }
}

statement {
  sid       = "ReadForTheBrake"
  effect    = "Allow"
  actions   = ["iam:ListAttachedRolePolicies", "iam:GetRole", "iam:GetPolicy"]
  resources = [local.plan_role_arn, local.apply_role_arn, aws_iam_policy.spend_brake.arn]
}
```

The second change matters independently of the first: today the executor's
`AttachRolePolicy` on the CI roles is unconditioned, so it can attach *any*
managed policy. That is what turns H-2 from "run as the executor" into
"attach AdministratorAccess to the apply role".

### H-2 — Boundary escape through trust-policy rewrite and `PassRole`

**Where.** `bootstrap/iam.tf:401-407` (`role_takeover_actions`, defined and
never referenced), `:1018-1019` (`iam:UpdateAssumeRolePolicy`, `iam:PassRole`
granted on `*`), `:595-606` (`DenyPassRoleExceptToAppServices`).

**What.** The comment above `role_takeover_actions` states the problem
precisely: "Taking over a pre-existing unbounded role is how a boundary-wearing
principal escapes: rewrite that role's trust policy to trust itself, assume
it, and the boundary no longer applies." The local was written and the
statement that would use it was not. Nothing in either policy document
references it (`grep role_takeover_actions bootstrap/` returns only the
definition).

The apply role has no `sts:AssumeRole`, so the direct version of the attack is
closed. The indirect one is open:

1. `iam:UpdateAssumeRolePolicy` on any role that lacks the boundary and is not
   one of the five control-plane ARNs — the `budgets-action` role is the
   concrete example in this account — replacing its trust policy with one that
   trusts `lambda.amazonaws.com`. Allowed: the action is granted on `*`, and no
   deny names it.
2. `lambda:CreateFunction --role <that role>`. `iam:PassRole` is granted on
   `*`; `DenyPassRoleExceptToAppServices` only asks *which service* receives
   the role, and Lambda is on the list. `DenyRoleWorkWithoutBoundary` does not
   cover `PassRole`. Allowed.
3. The function now executes with that role's permissions and **no guardrail
   and no boundary** — the guardrail is attached to the two CI roles, not to
   whatever they can pass. Against `budgets-action` that is
   `AttachRolePolicy` on the apply role with no policy restriction (see H-1),
   i.e. the apply role becomes Administrator minus the guardrail's denies.

The same three steps reach any other boundary-less role in the account that
sits outside the control-plane list. Identity Center's provisioned
`AWSReservedSSO_*` roles are the obvious candidates; I could not confirm from
this session whether AWS blocks trust-policy edits on those at the service
level, and the design should not depend on it either way.

**Why it matters.** `README.md:19` states the central guarantee: "escalate
privileges — the apply role holds no IAM role or policy write permission at
all, so there is no first step." The first half is no longer true (the role
manages application roles, correctly), and the second half was meant to be
preserved by the boundary condition. This is the gap in that preservation.

**Fix.** Two changes, and both are needed.

*Scope role management to roles this repository creates.* The `resources =
["*"]` on `ManageApplicationRoles` is what puts Identity Center roles and the
executor in reach. Every role and policy `infra/` creates is named
`${var.project}-…`, so:

```hcl
# bootstrap/iam.tf — data.aws_iam_policy_document.apply_permissions
statement {
  sid    = "ManageApplicationRoles"
  effect = "Allow"
  actions = [ …unchanged… ]
  # Named resources, not "*". Anything CI did not create — Identity Center's
  # roles, bootstrap's budgets-action role — is out of reach by construction
  # rather than by an enumerated deny.
  resources = [
    "${local.iam_root}:role/${var.project}-*",
    "${local.iam_root}:policy/${var.project}-*",
  ]
}
```

With that, the only boundary-less `${project}-*` roles are the two CI roles
and the executor, all of which H-1's change puts in the deny list.

*Wire the takeover deny, and confine `PassRole` the same way.* Add to the
guardrail:

```hcl
# bootstrap/iam.tf — data.aws_iam_policy_document.ci_guardrails
# Trust-policy and policy-attachment changes are only ever legitimate on a
# role that carries the boundary. DenyRoleWorkWithoutBoundary covers the
# attach/put half; this covers the takeover half, by name, because
# iam:UpdateAssumeRolePolicy does not support the iam:PermissionsBoundary
# condition key (stated from memory — verify against the IAM condition-key
# table before relying on it).
statement {
  sid           = "DenyRoleTakeoverOutsideApplicationRoles"
  effect        = "Deny"
  actions       = local.role_takeover_actions
  not_resources = ["${local.iam_root}:role/${var.project}-*"]
}

# PassRole is an escalation primitive whenever the role passed is stronger
# than the caller. Confining the target service is necessary and not
# sufficient; confine the role too.
statement {
  sid           = "DenyPassRoleOutsideApplicationRoles"
  effect        = "Deny"
  actions       = ["iam:PassRole"]
  not_resources = ["${local.iam_root}:role/${var.project}-*"]
}
```

And extend `DenyRoleWorkWithoutBoundary` to the remaining actions that do
support the boundary condition key — `iam:DetachRolePolicy` and
`iam:DeleteRolePolicy` — so a boundary-less role's policies cannot be stripped
either.

**One honest caveat about the naming approach.** After this change, a human who
creates a boundary-less role named `marcusdunnca-*` must add it to
`ci_control_plane_arns`. That is a rule to write in `bootstrap/iam.tf`'s header
comment; it is a much smaller surface than the current one.

## Medium

### M-1 — Application data is not protected from a compromised apply role

**Where.** `bootstrap/iam.tf:1000-1005` (`ManageApplicationServices`: `s3:*`,
`dynamodb:*` on `*`), `:1169-1184` (the boundary's
`DenyProtectedResourceDestruction`), `infra/dynamodb.tf:20-62`,
`infra/s3.tf:34-41`.

**What.** The permissions boundary denies `dynamodb:DeleteTable`,
`dynamodb:DeleteBackup` and `dynamodb:UpdateContinuousBackups` to any role CI
*creates*. The guardrail, which is what constrains the apply role itself, does
not. `DenyStateAndAuditDestruction` (`iam.tf:552-562`) protects the state and
trail buckets and nothing else. So the apply role can:

- `dynamodb:UpdateContinuousBackups` to disable point-in-time recovery, then
  `dynamodb:DeleteTable` (or simply `BatchWriteItem` every row away);
- `s3:DeleteObjectVersion` on every object in the documents bucket, which the
  30-day noncurrent-version lifecycle would otherwise leave recoverable, and
  `s3:DeleteBucket` after it.

`prevent_destroy` on both resources is a Terraform-side guard and stops nothing
done with the AWS CLI. `deletion_protection_enabled` is not set on the table.

**Why it matters.** `infra/dynamodb.tf:47-48`: "The reading history is the
entire point of the application and cannot be reconstructed." The README's
"destroy or ransom the record" line is about state and CloudTrail; the record
the application exists to keep has the weaker protection of the two.

**Fix.** Mirror the boundary's deny into the guardrail, extend it to the
documents bucket, and turn on deletion protection:

```hcl
# bootstrap/iam.tf — locals
docs_bucket_arn = "arn:${local.partition}:s3:::${var.project}-docs-${local.account_id}-${var.aws_region}-an"

# data.aws_iam_policy_document.ci_guardrails
statement {
  sid    = "DenyApplicationDataDestruction"
  effect = "Deny"
  actions = [
    "dynamodb:DeleteTable",
    "dynamodb:DeleteBackup",
    "dynamodb:UpdateContinuousBackups",
    "dynamodb:DeleteTableReplica",
  ]
  resources = ["*"]
}

statement {
  sid       = "DenyDocumentDestruction"
  effect    = "Deny"
  actions   = ["s3:DeleteBucket", "s3:DeleteObjectVersion", "s3:PutBucketVersioning", "s3:PutLifecycleConfiguration"]
  resources = [local.docs_bucket_arn, "${local.docs_bucket_arn}/*"]
}
```

```hcl
# infra/dynamodb.tf
deletion_protection_enabled = true
```

Deleting rows through the data plane stays possible — that is what the
application does — and PITR is the recovery for it, which is why
`UpdateContinuousBackups` is the load-bearing deny.

### M-2 — Unauthenticated writes against a 5 WCU table

**Where.** `app/api/src/main.rs:150-166` (the two public routes),
`app/api/src/auth.rs:93-111` (`start_challenge` writes a row),
`app/api/src/auth.rs:184` and `app/core/src/store.rs:980-999`
(`verify_assertion` issues a `DeleteItem` before any check), `infra/apigateway.tf:80-83`
(10 req/s sustained, aggregate), `infra/dynamodb.tf:24-25` (5 WCU / 5 RCU),
`app/core/src/store.rs:423-495` and `:623-657` (both list operations are table
scans).

**What.** Every `POST /auth/challenge` puts a ~1–2 KB row (the serialized
`PasskeyAuthentication` state) and every `POST /auth/verify` with a
syntactically valid body issues a `DeleteItem`, each 1–2 WCU. Neither route
needs a session. The gateway admits 10 req/s, so an attacker can drive 10–20
WCU/s against a table provisioned for 5. Burst credit covers a couple of
minutes; after that every write in the system — the owner's login, a quiz
submission, `generate` publishing questions — throttles.

The second effect outlasts the attack. The rows expire by TTL, which DynamoDB
documents as a sweep that can lag by up to 48 hours. One hour at the rate
limit leaves roughly 36,000 rows / ~50 MB in a table that `GET /docs` and
`GET /history` scan in full on every call. Fifty megabytes is ~6,000 read
units, or twenty minutes of the table's entire read capacity per document
list, until the sweep catches up. The projection on `list_docs` does not
help: scan cost is charged on items read, not items returned.

**Why it matters.** Denial of service, not denial of wallet — the capacity is
fixed, which is exactly what makes it exhaustible. The API Gateway note at
`apigateway.tf:71-79` explains the throttle was added to protect Lambda
concurrency; it does not bound what a request costs downstream.

**Fix.** Two independent parts; the first is the smaller change.

*Throttle the anonymous routes separately.* Declare them as explicit routes on
the same integration and give them their own limits, leaving `$default` for
everything else:

```hcl
# infra/apigateway.tf
resource "aws_apigatewayv2_route" "auth_challenge" {
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "POST /auth/challenge"
  target    = "integrations/${aws_apigatewayv2_integration.api.id}"
}
resource "aws_apigatewayv2_route" "auth_verify" {
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "POST /auth/verify"
  target    = "integrations/${aws_apigatewayv2_integration.api.id}"
}

# in aws_apigatewayv2_stage.default
route_settings {
  route_key              = "POST /auth/challenge"
  throttling_rate_limit  = 1
  throttling_burst_limit = 3
}
route_settings {
  route_key              = "POST /auth/verify"
  throttling_rate_limit  = 1
  throttling_burst_limit = 3
}
```

One login is two requests seconds apart; a human never notices 1 req/s.

*Keep ceremony state out of the scanned table.* Either a second, tiny table
for `AUTH` rows in on-demand mode (an attack at the throttled rate costs a few
cents an hour and touches no application capacity), or keep them in this
table but move the two list operations from `Scan` to `Query` against a
dedicated partition — the store's own comments already name that as the
migration trigger. The separate table is the smaller change and the one that
also removes the WCU contention.

### M-3 — Auto-merge reaches production code, and contradicts the README

**Where.** `.github/workflows/dependabot-auto-merge.yml:33-40`,
`.github/workflows/deploy.yml:11-19`, `README.md:306`.

**What.** The auto-merge condition is a denylist: everything that is not a
major bump and not `github_actions`. The comment explains why Actions are
excluded — "the one ecosystem whose code actually executes in CI". But
Dependabot security updates are enabled repository-wide
(`scripts/github-setup.sh`), so Cargo and npm advisories open PRs too, and
those:

- match the condition and get `gh pr merge --auto`;
- pass the "empty plan" gate trivially, because an app dependency never
  changes a `tofu plan`;
- bypass the 14-day cooldown by design (security updates are meant to);
- on merge, trigger `deploy.yml` on the `app/**` / `web/**` paths, which
  compiles the new crate (including its `build.rs`) and ships the result to
  the two Lambdas and the site bucket, in a job holding the apply role.

So the ecosystem the comment's reasoning most applies to is auto-merged
straight into production. The `nix` ecosystem is also caught by the
condition, while `README.md:306` says those bumps are merged by hand; and
provider bumps (`terraform`) are auto-merged even though the provider binary
is the thing that computes the plan the gate reads.

**Fix.** Make the condition an allowlist of the ecosystems for which the
empty-plan gate is actually meaningful, and say which in the README:

```yaml
if: >-
  steps.meta.outputs.update-type != 'version-update:semver-major' &&
  steps.meta.outputs.package-ecosystem == 'terraform'
```

Whether `terraform` belongs on that list is a judgement call worth writing
down: a provider release runs in the plan job on every PR regardless of
auto-merge, so the marginal exposure is the apply job. Everything else —
`cargo`, `npm_and_yarn`, `nix`, `github_actions` — should be read by a human,
which is the posture the README already describes.

### M-4 — No security response headers on the site

**Where.** `infra/cloudfront.tf:97-112` (no `response_headers_policy_id`),
`web/index.html` (no CSP meta), `web/src/lib/auth.ts:4-13` (session token in
`localStorage`, with the trade-off documented).

**What.** The distribution sends no `Strict-Transport-Security`,
`Content-Security-Policy`, `X-Frame-Options`/`frame-ancestors`,
`Referrer-Policy` or `X-Content-Type-Options` on the SPA (the API sets its own
`nosniff`/`no-store`). The `localStorage` choice is reasoned and the app
renders no third-party content, but it renders a great deal of model-generated
and user-typed text, and the React escaping that protects it today is a
single control. A CSP is the second one; it costs nothing at CloudFront.

**Fix.** A response-headers policy on the default behaviour. The only
non-`'self'` origins the app touches are the documents bucket (presigned
`PUT` from `uploadToS3`, presigned `GET` in the `<embed>` and link), so:

```hcl
resource "aws_cloudfront_response_headers_policy" "site" {
  name = "${var.project}-site"

  security_headers_config {
    strict_transport_security {
      access_control_max_age_sec = 31536000
      include_subdomains         = false   # the apex and other subdomains are not yours to promise for
      override                   = true
    }
    frame_options            { frame_option = "DENY"                override = true }
    content_type_options     {                                       override = true }
    referrer_policy          { referrer_policy = "strict-origin-when-cross-origin" override = true }
    content_security_policy {
      override = true
      content_security_policy = join("; ", [
        "default-src 'self'",
        "script-src 'self'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self' data:",
        "connect-src 'self' https://${aws_s3_bucket.docs.bucket_regional_domain_name}",
        "object-src https://${aws_s3_bucket.docs.bucket_regional_domain_name}",
        "frame-ancestors 'none'",
        "base-uri 'none'",
        "form-action 'self'",
      ])
    }
  }
}
```

Roll it out as `Content-Security-Policy-Report-Only` first: the `<embed>`'s
plugin and the presigned `PUT` are the two things most likely to need a
directive adjusted, and Vite's build must be confirmed to emit no inline
scripts (it should not, for this configuration).

## Low

### L-1 — Unpinned package manager in the deploy job

**Where.** `.github/workflows/deploy.yml:59`
(`corepack prepare pnpm@latest --activate`).

Everything else in this repository is pinned — actions to SHAs, providers and
nixpkgs to hashes, crates and npm packages to lockfiles — and the README says
so. `pnpm@latest` is resolved from the npm registry at deploy time and then
executes `pnpm install` and `pnpm build` in a job that holds the apply role. A
malicious or broken pnpm release is live here the day it is published.

**Fix.** Add a `packageManager` field with an integrity hash to
`web/package.json` (`corepack use pnpm@<version>` writes it); Corepack then
verifies the download. Drop `--activate pnpm@latest` in favour of plain
`corepack enable`. Dependabot will keep the pin current.

### L-2 — Registration token persists where any pull request can read it

**Where.** `infra/lambda.tf:355` (`REGISTRATION_TOKEN` as an environment
variable), `infra/variables.tf:213-238`, `bootstrap/iam.tf:364-370` (plan role:
`lambda:Get*`), `:691-695` (plan role: `s3:GetObjectVersion` on all state),
`bootstrap/state.tf:129` (90-day noncurrent retention).

The documented ceremony applies the token locally, and the next CI apply
clears it. During the window, `lambda:GetFunctionConfiguration` returns it to
the plan role — reachable from any PR — and it is written into
`infra/terraform.tfstate`, where the cleared value becomes a new version and
the old one stays readable through `GetObjectVersion` for 90 days.

The token is dead configuration once a credential exists, so this is only
dangerous if a token is ever reused or `WEBAUTHN_CREDENTIALS` is later
cleared. Both are plausible in an incident. **Fix:** treat every token as
burned after one ceremony (the docs nearly say this; make it explicit), or
better, move it to `/${project}/secret/registration-token` in SSM alongside
the signing key and have Terraform set only a boolean — then it never touches
Lambda configuration or state at all.

### L-3 — Claims that no longer hold

Three sentences the threat-model reader will rely on:

- `README.md:19`: "the apply role holds no IAM role or policy write permission
  at all". It now holds `ManageApplicationRoles` (`iam.tf:1010-1044`),
  bounded by `DenyRoleWorkWithoutBoundary`. The header of
  `data.aws_iam_policy_document.ci_permissions_boundary` (`iam.tf:1084-1088`)
  still says the boundary is "unused today". Rewrite both to describe the
  boundary mechanism, and H-2's fix.
- `README.md:132`: "Neither CI role can read them [secrets]". Directly true;
  transitively false. The apply role can create a boundary-carrying role, and
  the boundary grants `ssm:GetParameter` on `/${project}/*` including
  `/secret/*` (`iam.tf:1122-1127`). With `lambda:*` it can deploy a function
  under that role, or simply `UpdateFunctionCode` on the api function, and
  read the signing key. This is inherent to "CI deploys the application" and
  is fine to accept — but the README should say that the *plan* role cannot
  read secrets and the apply role can only do so by deploying code, which is
  recorded.
- `infra/apigateway.tf:29`: "Reached only through CloudFront at /api/*". The
  `execute-api` endpoint is public and nothing checks for the distribution.
  The stage throttle still applies, so the exposure is small; the sentence is
  wrong. See I-1.

### L-4 — Audit and document storage protections are thinner than state's

- `bootstrap/cloudtrail.tf:16-22`: the trail bucket relies on identity-policy
  denies alone; the state bucket has Object Lock in GOVERNANCE mode
  (`state.tf:104-115`) precisely so that immutability is "a property of the
  bucket rather than resting solely on an identity policy". Object Lock can
  be enabled on an existing versioned bucket and is free. Apply the same
  seven-day GOVERNANCE default to the trail bucket.
- `bootstrap/cloudtrail.tf:252-269`: data events are recorded for the state
  bucket only. Reads of uploaded PDFs — the only user data in the system —
  produce no audit record. At a handful of `GetObject`s a day the cost is
  negligible; add the documents bucket to the selector (never the trail
  bucket, per the comment there).

### L-5 — Policy hygiene

- `bootstrap/iam.tf:1041` grants `iam:CreateServiceLinkedRole` on `*` inside
  `ManageApplicationRoles`, which makes the conditioned statement at
  `:1049-1060` dead. Remove the unconditioned one; add service names to the
  conditioned statement as they become necessary.
- `bootstrap/iam.tf:262-297` (`account_control_actions`): the budget denies
  list `DeleteBudgetAction` but not `budgets:UpdateBudgetAction`,
  `budgets:CreateBudgetAction` or `budgets:ExecuteBudgetAction`. The apply
  role is not granted any of them today; the list exists so a future widening
  cannot re-enable them, and these three belong in it — `UpdateBudgetAction`
  can raise the brake's threshold to something it never reaches.
- `bootstrap/iam.tf:401-407`: `role_takeover_actions` is unreferenced (see
  H-2, which uses it).

## Informational

### I-1 — Direct gateway access and aggregate throttling

The HTTP API is reachable at its `execute-api` hostname without CloudFront.
The stage throttle (`apigateway.tf:80-83`) applies either way, and the
handler's CORS policy names the site origin exactly, so a hostile page gains
nothing. What is lost is only the distribution's TLS floor. HTTP APIs cannot
disable the default endpoint without a custom domain; a CloudFront
custom-origin header checked in the handler would close it if that is ever
wanted, at the cost of a shared secret in configuration.

The throttle is per stage, not per client, so 10 req/s from anyone returns
429 to the owner. This is the accepted cost of not paying for WAF and is
noted here so it is a decision rather than a discovery.

## Accepted risks I agree with

Recorded so the next reader does not re-open them.

- **Thirty-day non-revocable session token** (`auth.rs:19-27`). Rotating the
  SSM parameter invalidates every token; for one user that is the right
  revocation mechanism.
- **WebAuthn signature counter never updated** (`state.rs:252-260`). Synced
  passkeys report zero; enforcing the check would lock out the real
  authenticator. Clone detection is the only thing lost.
- **Registration mode as a distinct `Access` variant** (`state.rs:78-102`).
  Making "has credentials" and "serves enrolment" mutually exclusive in the
  type is the right shape; the length check and constant-time comparison on
  the token are correct.
- **Presigned upload with `content-length` in the signed set**
  (`docs.rs:133-201`). Fixing the size at signing is stronger than a range,
  and the note on why `createPresignedPost` is not available in the Rust SDK
  is accurate.
- **`PublicQuestion` as a separate type** (`model.rs:219-293`) with a test
  that asserts on serialized bytes (`docs.rs:1087-1109`). This is the
  correct way to make a leak a compile-time diff.
- **Voiding is gameable** (`model.rs:81-87`). One user, misleading themself
  on purpose; the reason is recorded. Agreed.

## Dependencies

`cargo audit` (RustSec database current as of this run; the yanked-crate check
failed on a registry 503 from this sandbox and is not reflected) reports five
advisories. `pnpm audit` reports none.

| Advisory | Crate | Pulled in by | Assessment |
| --- | --- | --- | --- |
| RUSTSEC-2026-0258 | `h2 0.3.27` | `aws-smithy-http-client 1.4.0`, via every AWS SDK crate in both Lambdas | Unbounded empty DATA frames — a DoS by a malicious HTTP/2 peer. The only HTTP/2 peers here are AWS endpoints, and under `behavior-version-latest` the SDK's default client is the hyper 1 / `h2 0.4.19` stack, which is fixed. The 0.3 line is compiled in for the SDK's legacy connector and appears not to be exercised. Low. |
| RUSTSEC-2026-0098, -0099, -0104 | `rustls-webpki 0.101.7` (via `rustls 0.21.12`) | same chain | Name-constraint and CRL-parsing defects in certificate validation. Same reasoning: the active TLS stack is `rustls 0.23.43` / `rustls-webpki 0.103.15`, which carries the fixes. Low. |
| RUSTSEC-2023-0071 | `rsa 0.9.10` | `jsonwebtoken 11` (`rust_crypto` feature) | Marvin timing side-channel on RSA private-key operations. This application signs and verifies HS256 only and never constructs an RSA key, so the affected code does not run. No upstream fix exists. Not applicable. |

**What to do about it.** None of these is a lockfile bump — the four in the
HTTP stack need `aws-smithy-http-client` to stop shipping its hyper 0.14
connector, or the SDK crates to be declared with `default-features = false`
plus only the hyper 1 client feature, which would drop `hyper 0.14`,
`rustls 0.21`, `rustls-webpki 0.101` and `h2 0.3` from the binary entirely
(and shrink the artifact the cold-start comments in `Cargo.toml` care about).
That is worth an afternoon. Note also that Dependabot cannot open a PR for a
transitive dependency whose parent has not released a fix, which is why
advisories dated April are still present in September: nothing in CI runs
`cargo audit`. A workflow that does, with no AWS permissions and a
hash-verified `cargo-audit` binary (the actions allowlist rules out the
marketplace action), is the cheap way to be told next time.

## Tests

`cargo test --workspace` from this checkout (the api crate linked against the
sandbox's system OpenSSL, as the local Nix shell does):

| Crate | Tests | Result |
| --- | --- | --- |
| `trainer-core` | 69 | all pass |
| `api` | 35 | all pass |
| `generate` | 26 | all pass |

The tests this audit leans on — `quiz_payload_cannot_contain_the_answer_key`,
`a_numeric_quiz_payload_cannot_contain_the_figure`,
`a_matching_token_is_accepted_and_nothing_else_is`,
`signing_and_verifying_a_token_does_not_panic`,
`traversal_and_nesting_are_refused`, `constant_time_eq_agrees_with_equality`
— are among them and pass.

The SPA has no test runner (`schemas.ts` notes issue #33). `pnpm install --frozen-lockfile`, `pnpm typecheck` (strict TypeScript) and `pnpm lint` (oxlint, correctness and suspicious rules as errors) all pass from this checkout.
