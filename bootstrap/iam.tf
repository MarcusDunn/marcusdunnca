# ---------------------------------------------------------------------------
# CI identities.
#
# Threat model: assume the CI pipeline is compromised — an attacker has write
# access to the repository and can therefore modify workflows, open and merge
# pull requests, and reach both roles. The goal is not that nothing happens; it
# is that the blast radius is small, bounded, and fully recorded.
#
# Consequences of that assumption, each implemented below:
#
#   * Neither role uses an AWS managed policy. AdministratorAccess and
#     ReadOnlyAccess are both far wider than this account needs, and AWS can
#     widen them further without notice. Every permission is enumerated.
#   * The apply role holds NO IAM role or policy write permission whatsoever.
#     It cannot create a role, attach a policy, or edit its own grants, so
#     privilege escalation has no first step.
#   * Everything is region-locked. The standard denial-of-wallet play is to
#     launch compute in every region simultaneously; here all but two regions
#     reject the call outright.
#   * Expensive service families are denied explicitly, on top of not being
#     allowlisted, so that carelessly widening the allowlist later cannot
#     silently re-enable them.
#   * State and audit history are append-mostly: object versions cannot be
#     deleted and logging cannot be stopped, so an attacker cannot encrypt or
#     erase the record of what they did.
#
# Expanding scope is meant to be a deliberate, reviewable act: add the specific
# actions to the allowlist below, in a pull request.
# ---------------------------------------------------------------------------

locals {
  plan_role_name  = "${var.project}-gha-plan"
  apply_role_name = "${var.project}-gha-apply"
  boundary_name   = "${var.project}-ci-permissions-boundary"
  guardrail_name  = "${var.project}-ci-guardrails"

  iam_root = "arn:${local.partition}:iam::${local.account_id}"

  plan_role_arn  = "${local.iam_root}:role/${local.plan_role_name}"
  apply_role_arn = "${local.iam_root}:role/${local.apply_role_name}"
  boundary_arn   = "${local.iam_root}:policy/${local.boundary_name}"
  guardrail_arn  = "${local.iam_root}:policy/${local.guardrail_name}"
  oidc_arn       = "${local.iam_root}:oidc-provider/token.actions.githubusercontent.com"

  # The things CI must never be able to touch: its own identities, the policies
  # that constrain it, and the trust anchor.
  ci_control_plane_arns = [
    local.plan_role_arn,
    local.apply_role_arn,
    local.boundary_arn,
    local.guardrail_arn,
    local.oidc_arn,
  ]

  state_bucket_arn = aws_s3_bucket.state.arn
  trail_bucket_arn = aws_s3_bucket.trail.arn

  # Immutable-form subject. See var.github_owner_id.
  github_sub_prefix = "repo:${var.github_owner}@${var.github_owner_id}/${var.github_repo}@${var.github_repo_id}"

  # Services whose API calls are not region-scoped, and so must be exempt from
  # the region lock or they would break entirely.
  global_service_actions = [
    "iam:*",
    "sts:*",
    "account:*",
    "organizations:*",
    "route53:*",
    "cloudfront:*",
    "support:*",
    "health:*",
    "budgets:*",
    "ce:*",
    "s3:ListAllMyBuckets",
    "s3:GetAccountPublicAccessBlock",
    "s3:PutAccountPublicAccessBlock",
  ]

  # Service families that can generate serious spend very quickly. None are
  # allowlisted, so these denies are redundant today — they exist so that a
  # future careless widening of the allowlist cannot quietly re-enable them.
  expensive_service_actions = [
    "ec2:RunInstances",
    "ec2:StartInstances",
    "ec2:RequestSpotInstances",
    "ec2:RequestSpotFleet",
    "ec2:CreateFleet",
    "ec2:CreateCapacityReservation",
    "ec2:PurchaseReservedInstancesOffering",
    "ec2:PurchaseCapacityBlock",
    "ec2:PurchaseHostReservation",
    "ec2:AllocateHosts",
    "savingsplans:*",
    "sagemaker:*",
    "bedrock:*",
    "emr:*",
    "elasticmapreduce:*",
    "redshift:*",
    "rds:*",
    "elasticache:*",
    "es:*",
    "opensearch:*",
    "eks:*",
    "ecs:*",
    "batch:*",
    "lightsail:*",
    "workspaces:*",
    "appstream:*",
    "braket:*",
    "quicksight:*",
    "glue:*",
    "kinesis:*",
    "kinesisanalytics:*",
    "mediaconvert:*",
    "medialive:*",
    "mediapackage:*",
    "datapipeline:*",
    "dms:*",
    "outposts:*",
    "snowball:*",
    "snowdevicemanagement:*",
    "aws-marketplace:Subscribe",
    "aws-marketplace:AcceptAgreementApprovalRequest",
    # CloudFront distribution create/update was removed from this list when the
    # application scope was agreed — it is now an allowlisted app service. Note
    # the consequence: CloudFront is global, so the region lock gives it no cost
    # containment, and data-transfer-out is genuinely unbounded. The $1 budget
    # and the $1 anomaly subscription are what bound it now.
    "cloudfront:CreateStreamingDistribution",
    "route53:CreateHostedZone",
    "route53domains:RegisterDomain",
    "route53domains:RenewDomain",
    "route53domains:TransferDomain",
  ]

  long_lived_credential_actions = [
    "iam:CreateUser",
    "iam:CreateAccessKey",
    "iam:CreateLoginProfile",
    "iam:UpdateLoginProfile",
    "iam:CreateServiceSpecificCredential",
    "iam:UploadSSHPublicKey",
    "iam:UploadSigningCertificate",
  ]

  # The AWS provider refreshes a bucket by reading every sub-resource, whether
  # or not this configuration sets it. All are read-only metadata calls — none
  # return object contents, so this does not widen data exposure.
  s3_bucket_read_actions = [
    "s3:GetBucketAcl",
    "s3:GetBucketCORS",
    "s3:GetBucketLocation",
    "s3:GetBucketLogging",
    "s3:GetBucketNotification",
    "s3:GetBucketObjectLockConfiguration",
    "s3:GetBucketOwnershipControls",
    "s3:GetBucketPolicy",
    "s3:GetBucketPolicyStatus",
    "s3:GetBucketPublicAccessBlock",
    "s3:GetBucketRequestPayment",
    "s3:GetBucketTagging",
    "s3:GetBucketVersioning",
    "s3:GetBucketWebsite",
    "s3:GetAccelerateConfiguration",
    "s3:GetAnalyticsConfiguration",
    "s3:GetEncryptionConfiguration",
    "s3:GetIntelligentTieringConfiguration",
    "s3:GetInventoryConfiguration",
    "s3:GetLifecycleConfiguration",
    "s3:GetMetricsConfiguration",
    "s3:GetReplicationConfiguration",
    "s3:ListBucket",
  ]

  # Shared Deny groups. The guardrail policy and the permissions boundary both
  # render from these.
  #
  # They were previously hand-copied, and the copies drifted: the boundary
  # silently omitted account control and half the destruction list, so a role
  # wearing it could close the account, hijack the console alias, silence every
  # spend alert, and bypass Object Lock. A boundary that permits more than the
  # guardrail is worse than none — it reads as a ceiling and is a hatch.
  state_audit_destruction_actions = [
    "s3:DeleteBucket",
    "s3:DeleteObjectVersion",
    "s3:PutBucketVersioning",
    "s3:PutBucketPolicy",
    "s3:DeleteBucketPolicy",
    "s3:PutEncryptionConfiguration",
    "s3:PutBucketPublicAccessBlock",
    "s3:PutLifecycleConfiguration",
    "s3:PutBucketObjectLockConfiguration",
    # Without these, Object Lock GOVERNANCE mode is decorative.
    "s3:BypassGovernanceRetention",
    "s3:PutObjectRetention",
    "s3:PutObjectLegalHold",
  ]

  audit_silencing_actions = [
    "cloudtrail:StopLogging",
    "cloudtrail:DeleteTrail",
    "cloudtrail:UpdateTrail",
    "cloudtrail:PutEventSelectors",
    "cloudtrail:DeleteEventDataStore",
    "cloudtrail:UpdateEventDataStore",
  ]

  key_destruction_actions = [
    "kms:ScheduleKeyDeletion",
    "kms:DisableKey",
    "kms:DisableKeyRotation",
    "kms:PutKeyPolicy",
    "kms:CreateGrant",
    "kms:RetireGrant",
    "kms:RevokeGrant",
  ]

  # Account-level control, plus the spend tripwires. budgets:* and ce:* are
  # exempt from the region lock, and silencing the alarm is the first move after
  # starting to burn money.
  account_control_actions = [
    "sso:*",
    "sso-directory:*",
    "identitystore:*",
    "organizations:*",
    "budgets:ModifyBudget",
    "budgets:UpdateBudget",
    "budgets:DeleteBudget",
    "budgets:DeleteBudgetAction",
    "budgets:DeleteNotification",
    "budgets:DeleteSubscriber",
    "budgets:UpdateSubscriber",
    "budgets:UpdateNotification",
    "ce:DeleteAnomalyMonitor",
    "ce:DeleteAnomalySubscription",
    "ce:UpdateAnomalyMonitor",
    "ce:UpdateAnomalySubscription",
    "ce:CreateAnomalySubscription",
    "sns:DeleteTopic",
    "sns:RemovePermission",
    "sns:SetTopicAttributes",
    # A FilterPolicy that drops everything leaves the subscription looking
    # healthy while delivering nothing.
    "sns:SetSubscriptionAttributes",
    "sns:Unsubscribe",
    "account:CloseAccount",
    "account:PutAlternateContact",
    "account:DeleteAlternateContact",
    "iam:DeleteAccountPasswordPolicy",
    "iam:UpdateAccountPasswordPolicy",
    "iam:CreateAccountAlias",
    "iam:DeleteAccountAlias",
    "s3:PutAccountPublicAccessBlock",
    "ec2:DisableEbsEncryptionByDefault",
    "access-analyzer:DeleteAnalyzer",
  ]

  # Services an application role may EVER touch, whatever policies get attached
  # to it. This is the whole point of the boundary: it is not a description of
  # what a role does, it is a ceiling on what any role created by CI could do
  # even if its inline policy said Action "*".
  #
  # Chosen to cover a small serverless app on free-tier-shaped services. logs is
  # not optional — Lambda cannot write anything without it. Adding to this list
  # widens every application role at once, so it deserves the same scrutiny as
  # widening the apply role itself.
  app_service_actions = [
    "lambda:*",
    "s3:*",
    "sqs:*",
    "sns:*",
    "logs:*",
    "cloudfront:*",
    "xray:PutTraceSegments",
    "xray:PutTelemetryRecords",
  ]

  # iam:PassRole is how a role gets handed to a service. Unconstrained it is a
  # privilege-escalation primitive — pass a powerful role to a service you
  # control and inherit it. Constrained to the services below, it is just
  # ordinary wiring.
  app_pass_role_services = [
    "lambda.amazonaws.com",
    "edgelambda.amazonaws.com",
  ]

  # Taking over a pre-existing unbounded role is how a boundary-wearing
  # principal escapes: rewrite that role's trust policy to trust itself, assume
  # it, and the boundary no longer applies. DenyRoleCreationWithoutThisBoundary
  # only covers roles being created.
  role_takeover_actions = [
    "iam:UpdateAssumeRolePolicy",
    "iam:AttachRolePolicy",
    "iam:PutRolePolicy",
    "iam:DetachRolePolicy",
    "iam:DeleteRolePolicy",
  ]

  iam_mutating_actions = [
    "iam:Create*",
    "iam:Delete*",
    "iam:Update*",
    "iam:Put*",
    "iam:Attach*",
    "iam:Detach*",
    "iam:Add*",
    "iam:Remove*",
    "iam:Set*",
    "iam:Tag*",
    "iam:Untag*",
  ]
}

# ---------------------------------------------------------------------------
# Trust policies
# ---------------------------------------------------------------------------

data "aws_iam_policy_document" "plan_assume_role" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github_actions.arn]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }

    # Note this subject is identical for a pull request opened from a fork —
    # GitHub does not distinguish them here. That is precisely why this role is
    # read-only and cannot read application data.
    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values   = ["${local.github_sub_prefix}:pull_request"]
    }
  }
}

data "aws_iam_policy_document" "apply_assume_role" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github_actions.arn]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values   = ["${local.github_sub_prefix}:environment:${var.apply_environment}"]
    }
  }
}

# ---------------------------------------------------------------------------
# Shared guardrails — attached to BOTH roles.
#
# These are pure Deny. An explicit Deny always beats any Allow, so nothing added
# to either role's allowlist later can override what is refused here.
# ---------------------------------------------------------------------------

data "aws_iam_policy_document" "ci_guardrails" {
  statement {
    sid         = "DenyAllOutsideAllowedRegions"
    effect      = "Deny"
    not_actions = local.global_service_actions
    resources   = ["*"]

    condition {
      test     = "StringNotEquals"
      variable = "aws:RequestedRegion"
      values   = var.allowed_regions
    }
  }

  statement {
    sid       = "DenyExpensiveServices"
    effect    = "Deny"
    actions   = local.expensive_service_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenyLongLivedCredentials"
    effect    = "Deny"
    actions   = local.long_lived_credential_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenyTamperingWithCIControlPlane"
    effect    = "Deny"
    actions   = local.iam_mutating_actions
    resources = local.ci_control_plane_arns
  }

  # Belt and braces. The apply role is not granted any of these in the first
  # place; this makes re-granting them impossible without also editing the
  # guardrail policy, which requires human-applied bootstrap access.
  statement {
    sid    = "DenyRoleAndPolicyCreation"
    effect = "Deny"
    actions = [
      # Role creation is NO LONGER denied outright — the application needs
      # execution roles. It is instead permitted only with the permissions
      # boundary attached, by DenyRoleWorkWithoutBoundary below. Removing a
      # boundary is still never allowed.
      "iam:DeleteRolePermissionsBoundary",
      "iam:PutUserPolicy",
      "iam:AttachUserPolicy",
      "iam:PutGroupPolicy",
      "iam:AttachGroupPolicy",
      "iam:CreateGroup",
      "iam:AddUserToGroup",
      "iam:CreateInstanceProfile",
      "iam:AddRoleToInstanceProfile",
    ]
    resources = ["*"]
  }


  # State is the map of the account and its version history is the record of
  # every change. Neither may be destroyed or re-encrypted under a key CI holds.
  # Covers the CloudTrail bucket too — the sid promised audit protection but the
  # resource list previously named only the state bucket, leaving log
  # destruction blocked merely by absence from the allowlist. One careless
  # future PR adding s3:Delete* would have silently re-opened it.
  statement {
    sid     = "DenyStateAndAuditDestruction"
    effect  = "Deny"
    actions = local.state_audit_destruction_actions
    resources = [
      local.state_bucket_arn,
      "${local.state_bucket_arn}/*",
      local.trail_bucket_arn,
      "${local.trail_bucket_arn}/*",
    ]
  }

  # DeleteObject is granted on the state bucket for the .tflock key, so it can
  # only be denied on the trail bucket. Previously the audit log was protected
  # from DeleteObjectVersion but not from a plain delete marker.
  # The control that makes iam:CreateRole safe to grant.
  #
  # Every role CI creates must carry the permissions boundary, which caps it at
  # local.app_service_actions no matter what policy is attached. On CreateRole
  # the condition key holds the boundary being set; if none is set the key is
  # absent, and an absent key makes StringNotEquals TRUE — so the deny fires.
  # No boundary, no role.
  statement {
    sid    = "DenyRoleWorkWithoutBoundary"
    effect = "Deny"
    actions = [
      "iam:CreateRole",
      "iam:PutRolePolicy",
      "iam:AttachRolePolicy",
      "iam:PutRolePermissionsBoundary",
    ]
    resources = ["${local.iam_root}:role/*"]

    condition {
      test     = "StringNotEquals"
      variable = "iam:PermissionsBoundary"
      values   = [local.boundary_arn]
    }
  }

  # PassRole unconstrained is an escalation primitive: hand a powerful role to a
  # service you control and inherit it. Constrained to the app services it is
  # ordinary wiring. A missing iam:PassedToService key fails closed here too.
  statement {
    sid       = "DenyPassRoleExceptToAppServices"
    effect    = "Deny"
    actions   = ["iam:PassRole"]
    resources = ["*"]

    condition {
      test     = "StringNotEquals"
      variable = "iam:PassedToService"
      values   = local.app_pass_role_services
    }
  }

  statement {
    sid       = "DenyDeletingAuditLogs"
    effect    = "Deny"
    actions   = ["s3:DeleteObject"]
    resources = ["${local.trail_bucket_arn}/*"]
  }

  # Defence in depth behind the key-scoped grant above: even if the apply
  # allowlist is widened later, bootstrap's state stays unwritable by CI.
  statement {
    sid    = "DenyWritingBootstrapState"
    effect = "Deny"
    actions = [
      "s3:PutObject",
      "s3:DeleteObject",
    ]
    resources = ["${local.state_bucket_arn}/bootstrap/*"]
  }

  statement {
    sid       = "DenySilencingTheAuditTrail"
    effect    = "Deny"
    actions   = local.audit_silencing_actions
    resources = ["*"]
  }

  # Ransomware defence: a key that cannot be deleted or disabled cannot be used
  # to hold data hostage, and a policy that cannot be rewritten cannot be
  # narrowed to an attacker-held principal.
  statement {
    sid       = "DenyKeyDestruction"
    effect    = "Deny"
    actions   = local.key_destruction_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenyAccountAndIdentityCenterControl"
    effect    = "Deny"
    actions   = local.account_control_actions
    resources = ["*"]
  }
}

resource "aws_iam_policy" "ci_guardrails" {
  name        = local.guardrail_name
  description = "Deny-only guardrails attached to both CI roles. Explicit Deny beats any Allow, so these bound the blast radius of a compromised pipeline."
  policy      = data.aws_iam_policy_document.ci_guardrails.json
}

# ---------------------------------------------------------------------------
# Plan role — enumerated read-only.
#
# Deliberately NOT ReadOnlyAccess. That managed policy grants s3:GetObject on
# every bucket in the account, so a compromised plan job could exfiltrate all
# application data. Here object reads are confined to the state bucket.
# ---------------------------------------------------------------------------

data "aws_iam_policy_document" "plan_permissions" {
  statement {
    sid       = "ReadState"
    effect    = "Allow"
    actions   = ["s3:GetObject", "s3:GetObjectVersion"]
    resources = ["${local.state_bucket_arn}/*"]
  }

  statement {
    sid       = "ListStateBucket"
    effect    = "Allow"
    actions   = ["s3:ListBucket", "s3:GetBucketLocation"]
    resources = [local.state_bucket_arn]
  }

  # Describe-level reads needed to refresh the resources this repo manages.
  # None of these return object contents or secret material.
  # Bucket metadata for the two buckets this repo manages. Scoped to those
  # buckets rather than "*", so a compromised plan job cannot enumerate the
  # configuration of buckets the application may hold later.
  statement {
    sid       = "ReadManagedBucketMetadata"
    effect    = "Allow"
    actions   = local.s3_bucket_read_actions
    resources = [local.state_bucket_arn, local.trail_bucket_arn]
  }

  # IAM reads scoped to the four objects this repo manages. Previously these
  # were Resource "*", so a compromised plan job could enumerate every policy
  # in the account — well beyond "reads needed to refresh what this repo owns".
  statement {
    sid    = "ReadManagedIAMObjects"
    effect = "Allow"
    actions = [
      "iam:GetRole",
      "iam:GetRolePolicy",
      "iam:ListRolePolicies",
      "iam:ListAttachedRolePolicies",
      "iam:ListRoleTags",
      "iam:GetPolicy",
      "iam:GetPolicyVersion",
      "iam:ListPolicyVersions",
    ]
    # Widened from the four managed objects to all roles and policies, because
    # CI now creates application execution roles and `tofu plan` must refresh
    # them. Still read-only, and still excludes users and groups (there are
    # none, and creating them is denied).
    resources = [
      "${local.iam_root}:role/*",
      "${local.iam_root}:policy/*",
    ]
  }

  statement {
    sid    = "DescribeManagedResources"
    effect = "Allow"
    actions = [
      "sts:GetCallerIdentity",
      "iam:GetOpenIDConnectProvider",
      "iam:ListOpenIDConnectProviders",
      "iam:ListAccountAliases",
      "iam:GetAccountPasswordPolicy",
      # Per-bucket metadata is granted separately, scoped to the two managed
      # buckets. These two are account-level and have no bucket to scope to.
      "s3:GetAccountPublicAccessBlock",
      "s3:ListAllMyBuckets",
      "cloudtrail:DescribeTrails",
      "cloudtrail:GetTrail",
      "cloudtrail:GetTrailStatus",
      "cloudtrail:GetEventSelectors",
      "cloudtrail:GetInsightSelectors",
      "cloudtrail:ListTags",
      "access-analyzer:GetAnalyzer",
      "access-analyzer:ListAnalyzers",
      "access-analyzer:ListTagsForResource",
      "ec2:GetEbsEncryptionByDefault",
      "ec2:GetEbsDefaultKmsKeyId",
      "account:GetAlternateContact",
      # Cost tripwires managed in bootstrap/. Read-only: these return budget
      # thresholds and monitor configuration, not spend data or contents.
      "budgets:ViewBudget",
      "budgets:DescribeBudget",
      "budgets:DescribeBudgetAction",
      "budgets:ListTagsForResource",
      "ce:GetAnomalyMonitors",
      "ce:GetAnomalySubscriptions",
      "ce:ListTagsForResource",
      "sns:GetTopicAttributes",
      "sns:ListTagsForResource",
      "sns:GetSubscriptionAttributes",
      "sns:ListSubscriptionsByTopic",
      "kms:DescribeKey",
      "kms:GetKeyRotationStatus",
      "kms:GetKeyPolicy",
      "kms:ListAliases",
      "kms:ListResourceTags",
      "tag:GetResources",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_role" "plan" {
  name                 = local.plan_role_name
  description          = "Enumerated read-only role assumed by pull_request jobs to run `tofu plan`."
  assume_role_policy   = data.aws_iam_policy_document.plan_assume_role.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy" "plan_permissions" {
  name   = "permissions"
  role   = aws_iam_role.plan.id
  policy = data.aws_iam_policy_document.plan_permissions.json
}

resource "aws_iam_role_policy_attachment" "plan_guardrails" {
  role       = aws_iam_role.plan.name
  policy_arn = aws_iam_policy.ci_guardrails.arn
}

# ---------------------------------------------------------------------------
# Apply role — enumerated write access, deliberately narrow.
#
# The account's security floor (CloudTrail, public access block, password
# policy, EBS encryption default, alternate contacts) lives in this bootstrap
# module and is applied by a human. CI is not granted permission to change any
# of it, so a compromised pipeline cannot weaken the account's defences or hide
# what it did. CI owns operational and, in future, application resources.
# ---------------------------------------------------------------------------

data "aws_iam_policy_document" "apply_permissions" {
  # State backend, scoped to the EXACT keys infra/ uses.
  #
  # This was `${state_bucket_arn}/*`, which included bootstrap/terraform.tfstate
  # and was a critical hole. bootstrap/ is applied by a human holding
  # AdministratorAccess, and OpenTofu gathers provider requirements from STATE,
  # not only from configuration. A compromised CI job could therefore write a
  # reference to an attacker-published provider into bootstrap's state; the next
  # `scripts/bootstrap.sh` run would download and execute that provider binary
  # during `init`/`plan` — remote code execution on the operator's workstation,
  # inside a live admin session, before the apply prompt is ever shown. The
  # committed .terraform.lock.hcl does not prevent this; init simply adds the
  # state-referenced provider to it.
  #
  # Enumerating the two keys also removes CI's ability to use the state bucket
  # as an unbounded blob store, which was a denial-of-wallet path through an
  # otherwise-allowed action.
  statement {
    sid    = "ReadWriteInfraState"
    effect = "Allow"
    actions = [
      "s3:GetObject",
      "s3:GetObjectVersion",
      "s3:PutObject",
    ]
    resources = [
      "${local.state_bucket_arn}/infra/terraform.tfstate",
      "${local.state_bucket_arn}/infra/terraform.tfstate.tflock",
    ]
  }

  # DeleteObject is needed ONLY to release the state lock. Granting it on the
  # state object too let CI put a delete marker over live infra state — a free
  # disruption primitive with no operational purpose. Recoverable, since prior
  # versions are Object-Lock retained, but pointless to allow.
  statement {
    sid       = "ReleaseStateLock"
    effect    = "Allow"
    actions   = ["s3:DeleteObject"]
    resources = ["${local.state_bucket_arn}/infra/terraform.tfstate.tflock"]
  }

  statement {
    sid       = "ListStateBucket"
    effect    = "Allow"
    actions   = ["s3:ListBucket", "s3:GetBucketLocation", "s3:GetBucketVersioning"]
    resources = [local.state_bucket_arn]
  }

  statement {
    sid     = "Identity"
    effect  = "Allow"
    actions = ["sts:GetCallerIdentity", "tag:GetResources"]

    resources = ["*"]
  }

  # Everything CI currently manages in infra/. Expand this list, in a pull
  # request, when the application needs more.
  statement {
    sid    = "ManageOperationalResources"
    effect = "Allow"
    actions = [
      "access-analyzer:GetAnalyzer",
      "access-analyzer:ListAnalyzers",
      "access-analyzer:ListTagsForResource",
      "access-analyzer:CreateAnalyzer",
      "access-analyzer:TagResource",
      "access-analyzer:UntagResource",
      # Read-only. The alias itself is owned by bootstrap — see
      # DenyAccountAliasHijack for why CI must not be able to release it.
      "iam:ListAccountAliases",
    ]
    resources = ["*"]
  }

  # Application infrastructure. Same list the boundary caps created roles at, so
  # CI cannot build something it could not also grant a role access to.
  statement {
    sid       = "ManageApplicationServices"
    effect    = "Allow"
    actions   = local.app_service_actions
    resources = ["*"]
  }

  # Execution roles for the application. Safe to grant only because
  # DenyRoleWorkWithoutBoundary forces every one of them to carry the boundary,
  # and DenyPassRoleExceptToAppServices confines where they can be handed.
  statement {
    sid    = "ManageApplicationRoles"
    effect = "Allow"
    actions = [
      "iam:CreateRole",
      "iam:DeleteRole",
      "iam:GetRole",
      "iam:UpdateRole",
      "iam:UpdateAssumeRolePolicy",
      "iam:PassRole",
      "iam:PutRolePolicy",
      "iam:DeleteRolePolicy",
      "iam:GetRolePolicy",
      "iam:ListRolePolicies",
      "iam:AttachRolePolicy",
      "iam:DetachRolePolicy",
      "iam:ListAttachedRolePolicies",
      "iam:PutRolePermissionsBoundary",
      "iam:TagRole",
      "iam:UntagRole",
      "iam:ListRoleTags",
      "iam:CreatePolicy",
      "iam:DeletePolicy",
      "iam:CreatePolicyVersion",
      "iam:DeletePolicyVersion",
      "iam:GetPolicy",
      "iam:GetPolicyVersion",
      "iam:ListPolicyVersions",
      "iam:ListEntitiesForPolicy",
      "iam:TagPolicy",
      "iam:UntagPolicy",
      "iam:CreateServiceLinkedRole",
    ]
    resources = ["*"]
  }

  # Access Analyzer provisions its own service-linked role on first use. This is
  # narrowly conditioned so it cannot be used to create a service-linked role
  # for any other service.
  statement {
    sid       = "AccessAnalyzerServiceLinkedRole"
    effect    = "Allow"
    actions   = ["iam:CreateServiceLinkedRole"]
    resources = ["*"]

    condition {
      test     = "StringEquals"
      variable = "iam:AWSServiceName"
      values   = ["access-analyzer.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "apply" {
  name                 = local.apply_role_name
  description          = "Enumerated write role assumed by main-branch apply jobs via the protected `${var.apply_environment}` environment."
  assume_role_policy   = data.aws_iam_policy_document.apply_assume_role.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy" "apply_permissions" {
  name   = "permissions"
  role   = aws_iam_role.apply.id
  policy = data.aws_iam_policy_document.apply_permissions.json
}

resource "aws_iam_role_policy_attachment" "apply_guardrails" {
  role       = aws_iam_role.apply.name
  policy_arn = aws_iam_policy.ci_guardrails.arn
}

# ---------------------------------------------------------------------------
# Permissions boundary.
#
# Unused today: the apply role cannot create roles at all, which is a stronger
# guarantee than bounding what the roles it creates may do. Kept because the
# moment iam:CreateRole is added to the allowlist — when the application needs
# an execution role — the boundary and the condition enforcing it must already
# exist, or that expansion silently becomes an escalation path.
# ---------------------------------------------------------------------------

data "aws_iam_policy_document" "ci_permissions_boundary" {
  # The ceiling. A role wearing this boundary can never act outside these
  # services, regardless of what policy CI attaches to it. This replaced a
  # blanket Allow "*" — which made the boundary a formality rather than a limit.
  statement {
    sid       = "AllowOnlyApplicationServices"
    effect    = "Allow"
    actions   = local.app_service_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenyLongLivedCredentials"
    effect    = "Deny"
    actions   = local.long_lived_credential_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenyExpensiveServices"
    effect    = "Deny"
    actions   = local.expensive_service_actions
    resources = ["*"]
  }

  statement {
    sid    = "DenyBoundaryEscape"
    effect = "Deny"
    actions = [
      "iam:DeleteRolePermissionsBoundary",
      "iam:DeleteUserPermissionsBoundary",
      "iam:PutRolePermissionsBoundary",
      "iam:PutUserPermissionsBoundary",
    ]
    resources = ["*"]
  }

  # Without this the boundary was an escape hatch, not a ceiling: a principal
  # holding it could iam:CreateRole a NEW, unbounded role and attach
  # AdministratorAccess to it. DenyBoundaryEscape above only stops removing a
  # boundary from an existing principal — it never required a new one to carry
  # the boundary in the first place. A missing iam:PermissionsBoundary key makes
  # StringNotEquals true, so no-boundary fails closed.
  statement {
    sid    = "DenyRoleCreationWithoutThisBoundary"
    effect = "Deny"
    actions = [
      "iam:CreateRole",
      "iam:PutRolePermissionsBoundary",
    ]
    resources = ["*"]

    condition {
      test     = "StringNotEquals"
      variable = "iam:PermissionsBoundary"
      values   = [local.boundary_arn]
    }
  }

  # The boundary previously omitted the region lock and the destruction denies
  # entirely, so a boundary-constrained role was free in every region and could
  # delete state versions and silence the trail. Reuse the same statements the
  # guardrail applies to the CI roles themselves.
  statement {
    sid         = "DenyAllOutsideAllowedRegions"
    effect      = "Deny"
    not_actions = local.global_service_actions
    resources   = ["*"]

    condition {
      test     = "StringNotEquals"
      variable = "aws:RequestedRegion"
      values   = var.allowed_regions
    }
  }

  # Rendered from the same lists as the guardrail. "*" rather than the bucket
  # ARNs: a role wearing this boundary has no legitimate reason to destroy
  # state, logs, or keys anywhere in the account.
  # The control that makes iam:CreateRole safe to grant.
  #
  # PassRole unconstrained is an escalation primitive: hand a powerful role to a
  # service you control and inherit it. Constrained to the app services, it is
  # ordinary wiring. A missing iam:PassedToService key fails closed here too.
  statement {
    sid       = "DenyPassRoleExceptToAppServices"
    effect    = "Deny"
    actions   = ["iam:PassRole"]
    resources = ["*"]

    condition {
      test     = "StringNotEquals"
      variable = "iam:PassedToService"
      values   = local.app_pass_role_services
    }
  }

  statement {
    sid       = "DenyStateAndAuditDestruction"
    effect    = "Deny"
    actions   = local.state_audit_destruction_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenySilencingTheAuditTrail"
    effect    = "Deny"
    actions   = local.audit_silencing_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenyKeyDestruction"
    effect    = "Deny"
    actions   = local.key_destruction_actions
    resources = ["*"]
  }

  # This whole group was absent. Most starkly, iam:DeleteAccountAlias was
  # permitted — the boundary allowed the exact permanent console-URL hijack the
  # guardrail spends a paragraph explaining.
  statement {
    sid       = "DenyAccountAndIdentityCenterControl"
    effect    = "Deny"
    actions   = local.account_control_actions
    resources = ["*"]
  }

  # Closes the escape where a boundary-wearing role rewrites a pre-existing
  # unbounded role's trust policy to trust itself, assumes it, and steps out.
  statement {
    sid       = "DenyRoleTakeover"
    effect    = "Deny"
    actions   = local.role_takeover_actions
    resources = ["*"]
  }

  statement {
    sid       = "DenyTamperingWithCIControlPlane"
    effect    = "Deny"
    actions   = local.iam_mutating_actions
    resources = local.ci_control_plane_arns
  }
}

resource "aws_iam_policy" "ci_permissions_boundary" {
  name        = local.boundary_name
  description = "Maximum privilege any future CI-created role may hold. Not yet in use — the apply role cannot create roles."
  policy      = data.aws_iam_policy_document.ci_permissions_boundary.json
}
