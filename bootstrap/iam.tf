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
      "iam:CreateRole",
      "iam:CreatePolicy",
      "iam:CreatePolicyVersion",
      "iam:PutRolePolicy",
      "iam:AttachRolePolicy",
      "iam:UpdateAssumeRolePolicy",
      "iam:DeleteRolePermissionsBoundary",
      "iam:PutRolePermissionsBoundary",
    ]
    resources = ["*"]
  }

  # State is the map of the account and its version history is the record of
  # every change. Neither may be destroyed or re-encrypted under a key CI holds.
  statement {
    sid    = "DenyStateAndAuditDestruction"
    effect = "Deny"
    actions = [
      "s3:DeleteBucket",
      "s3:DeleteObjectVersion",
      "s3:PutBucketVersioning",
      "s3:PutBucketPolicy",
      "s3:DeleteBucketPolicy",
      "s3:PutEncryptionConfiguration",
      "s3:PutBucketPublicAccessBlock",
      "s3:PutLifecycleConfiguration",
    ]
    resources = [
      local.state_bucket_arn,
      "${local.state_bucket_arn}/*",
    ]
  }

  statement {
    sid    = "DenySilencingTheAuditTrail"
    effect = "Deny"
    actions = [
      "cloudtrail:StopLogging",
      "cloudtrail:DeleteTrail",
      "cloudtrail:UpdateTrail",
      "cloudtrail:PutEventSelectors",
      "cloudtrail:DeleteEventDataStore",
    ]
    resources = ["*"]
  }

  # Ransomware defence: a key that cannot be deleted or disabled cannot be used
  # to hold data hostage, and a policy that cannot be rewritten cannot be
  # narrowed to an attacker-held principal.
  statement {
    sid    = "DenyKeyDestruction"
    effect = "Deny"
    actions = [
      "kms:ScheduleKeyDeletion",
      "kms:DisableKey",
      "kms:DisableKeyRotation",
      "kms:PutKeyPolicy",
      "kms:CreateGrant",
      "kms:RetireGrant",
      "kms:RevokeGrant",
    ]
    resources = ["*"]
  }

  statement {
    sid    = "DenyAccountAndIdentityCenterControl"
    effect = "Deny"
    actions = [
      "sso:*",
      "sso-directory:*",
      "identitystore:*",
      "organizations:*",
      "account:CloseAccount",
      "account:PutAlternateContact",
      "account:DeleteAlternateContact",
      "iam:DeleteAccountPasswordPolicy",
      "iam:UpdateAccountPasswordPolicy",
      "s3:PutAccountPublicAccessBlock",
      "ec2:DisableEbsEncryptionByDefault",
      "access-analyzer:DeleteAnalyzer",
    ]
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
  statement {
    sid    = "DescribeManagedResources"
    effect = "Allow"
    actions = [
      "sts:GetCallerIdentity",
      "iam:GetRole",
      "iam:GetRolePolicy",
      "iam:GetPolicy",
      "iam:GetPolicyVersion",
      "iam:ListRolePolicies",
      "iam:ListAttachedRolePolicies",
      "iam:ListRoleTags",
      "iam:ListPolicyVersions",
      "iam:GetOpenIDConnectProvider",
      "iam:ListOpenIDConnectProviders",
      "iam:ListAccountAliases",
      "iam:GetAccountPasswordPolicy",
      "s3:GetBucketVersioning",
      "s3:GetBucketPolicy",
      "s3:GetBucketPublicAccessBlock",
      "s3:GetBucketOwnershipControls",
      "s3:GetBucketTagging",
      "s3:GetBucketLogging",
      "s3:GetBucketObjectLockConfiguration",
      "s3:GetEncryptionConfiguration",
      "s3:GetLifecycleConfiguration",
      "s3:GetAccelerateConfiguration",
      "s3:GetReplicationConfiguration",
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
  # State backend. DeleteObject is required to release the S3 native lock.
  statement {
    sid    = "ReadWriteState"
    effect = "Allow"
    actions = [
      "s3:GetObject",
      "s3:GetObjectVersion",
      "s3:PutObject",
      "s3:DeleteObject",
    ]
    resources = ["${local.state_bucket_arn}/*"]
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
      "iam:ListAccountAliases",
      "iam:CreateAccountAlias",
      "iam:DeleteAccountAlias",
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
  statement {
    sid       = "AllowServiceAccessByDefault"
    effect    = "Allow"
    actions   = ["*"]
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
