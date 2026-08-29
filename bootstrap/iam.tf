# ---------------------------------------------------------------------------
# CI identities.
#
# Two roles, deliberately asymmetric:
#
#   plan  — read-only, assumable by any pull_request job. Runs untrusted-ish
#           branch code, so it can observe the account but change nothing.
#   apply — privileged, assumable ONLY by a job declaring the protected
#           `production` GitHub Environment. That environment is pinned to main,
#           so the trust policy itself enforces "apply only from main".
#
# ARNs below are built from locals rather than resource references. The boundary
# policy names the roles, the roles' guardrails name the boundary — referencing
# the resources directly would be a dependency cycle. Names are deterministic,
# so string-building is safe here.
# ---------------------------------------------------------------------------

locals {
  plan_role_name  = "${var.project}-gha-plan"
  apply_role_name = "${var.project}-gha-apply"
  boundary_name   = "${var.project}-ci-permissions-boundary"

  iam_root = "arn:${local.partition}:iam::${local.account_id}"

  plan_role_arn  = "${local.iam_root}:role/${local.plan_role_name}"
  apply_role_arn = "${local.iam_root}:role/${local.apply_role_name}"
  boundary_arn   = "${local.iam_root}:policy/${local.boundary_name}"
  oidc_arn       = "${local.iam_root}:oidc-provider/token.actions.githubusercontent.com"

  # The four things CI must never be able to touch: its own two identities, the
  # boundary that constrains everything it creates, and the trust anchor itself.
  ci_control_plane_arns = [
    local.plan_role_arn,
    local.apply_role_arn,
    local.boundary_arn,
    local.oidc_arn,
  ]

  # Mutating IAM verbs. Used instead of a blunt `iam:*` deny so that read calls
  # (GetRole, ListAttachedRolePolicies) still work during plan/refresh.
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

  # Credential types that outlive a CI job. Creating any of these would defeat
  # the entire point of OIDC federation.
  long_lived_credential_actions = [
    "iam:CreateUser",
    "iam:CreateAccessKey",
    "iam:CreateLoginProfile",
    "iam:UpdateLoginProfile",
    "iam:CreateServiceSpecificCredential",
    "iam:UploadSSHPublicKey",
    "iam:UploadSigningCertificate",
  ]

  github_sub_prefix = "repo:${var.github_owner}/${var.github_repo}"
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

    # PR tokens carry exactly this sub — no branch component, which is fine
    # because this role cannot change anything.
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

    # The environment claim is the whole security boundary for apply. A job that
    # does not declare `environment: production` gets a token whose sub does not
    # match, and STS refuses it. Combined with the environment's branch
    # restriction on the GitHub side, apply is reachable only from main.
    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values   = ["${local.github_sub_prefix}:environment:${var.apply_environment}"]
    }
  }
}

# ---------------------------------------------------------------------------
# Permissions boundary applied to every role CI creates
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

  statement {
    sid    = "DenyAuditAndAccountControl"
    effect = "Deny"
    actions = [
      "cloudtrail:StopLogging",
      "cloudtrail:DeleteTrail",
      "cloudtrail:DeleteEventDataStore",
      "sso:*",
      "sso-directory:*",
      "identitystore:*",
      "organizations:LeaveOrganization",
      "account:CloseAccount",
    ]
    resources = ["*"]
  }

  statement {
    sid    = "DenyStateBucketControl"
    effect = "Deny"
    actions = [
      "s3:PutBucketPolicy",
      "s3:DeleteBucketPolicy",
      "s3:DeleteBucket",
      "s3:PutBucketVersioning",
      "s3:PutBucketPublicAccessBlock",
      "s3:PutEncryptionConfiguration",
      "s3:DeleteObjectVersion",
    ]
    resources = [
      aws_s3_bucket.state.arn,
      "${aws_s3_bucket.state.arn}/*",
    ]
  }
}

resource "aws_iam_policy" "ci_permissions_boundary" {
  name        = local.boundary_name
  description = "Maximum privilege any CI-created role may hold. Enforced on CreateRole by the apply role's guardrails."
  policy      = data.aws_iam_policy_document.ci_permissions_boundary.json
}

# ---------------------------------------------------------------------------
# Plan role — read-only
# ---------------------------------------------------------------------------

resource "aws_iam_role" "plan" {
  name                 = local.plan_role_name
  description          = "Read-only role assumed by pull_request jobs to run `tofu plan`."
  assume_role_policy   = data.aws_iam_policy_document.plan_assume_role.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy_attachment" "plan_readonly" {
  role       = aws_iam_role.plan.name
  policy_arn = "arn:${local.partition}:iam::aws:policy/ReadOnlyAccess"
}

data "aws_iam_policy_document" "plan_extra_denies" {
  # ReadOnlyAccess does not grant GetSecretValue today, but it is a managed
  # policy AWS can widen at any time. Plan jobs run branch code from PRs, so
  # nail this shut rather than trusting AWS's future edits.
  statement {
    sid       = "DenySecretMaterial"
    effect    = "Deny"
    actions   = ["secretsmanager:GetSecretValue"]
    resources = ["*"]
  }
}

resource "aws_iam_role_policy" "plan_extra_denies" {
  name   = "extra-denies"
  role   = aws_iam_role.plan.id
  policy = data.aws_iam_policy_document.plan_extra_denies.json
}

# ---------------------------------------------------------------------------
# Apply role — privileged, but fenced in
# ---------------------------------------------------------------------------

resource "aws_iam_role" "apply" {
  name                 = local.apply_role_name
  description          = "Privileged role assumed by main-branch apply jobs via the protected `${var.apply_environment}` environment."
  assume_role_policy   = data.aws_iam_policy_document.apply_assume_role.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy_attachment" "apply_admin" {
  role       = aws_iam_role.apply.name
  policy_arn = "arn:${local.partition}:iam::aws:policy/AdministratorAccess"
}

data "aws_iam_policy_document" "apply_guardrails" {
  # Explicit Deny beats the AdministratorAccess Allow, so everything below holds
  # regardless of what the managed policy grants now or later.

  statement {
    sid       = "DenyTamperingWithCIControlPlane"
    effect    = "Deny"
    actions   = local.iam_mutating_actions
    resources = local.ci_control_plane_arns
  }

  statement {
    sid       = "DenyLongLivedCredentials"
    effect    = "Deny"
    actions   = local.long_lived_credential_actions
    resources = ["*"]
  }

  # Every role CI creates must carry the boundary. On CreateRole the condition
  # key holds the boundary being set; if none is set the key is absent, and an
  # absent key makes StringNotEquals true — so the deny fires. That is exactly
  # the behaviour we want: no boundary, no role.
  statement {
    sid    = "DenyRoleWorkWithoutPermissionsBoundary"
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

  statement {
    sid    = "DenyBoundaryRemoval"
    effect = "Deny"
    actions = [
      "iam:DeleteRolePermissionsBoundary",
      "iam:DeleteUserPermissionsBoundary",
    ]
    resources = ["*"]
  }

  # The state bucket is bootstrap-owned. CI reads and writes state objects, but
  # must not be able to reconfigure the bucket or erase state history — that is
  # the forensic record of every change it has ever made.
  statement {
    sid    = "DenyStateBucketControl"
    effect = "Deny"
    actions = [
      "s3:PutBucketPolicy",
      "s3:DeleteBucketPolicy",
      "s3:DeleteBucket",
      "s3:PutBucketVersioning",
      "s3:PutBucketPublicAccessBlock",
      "s3:PutEncryptionConfiguration",
      "s3:PutLifecycleConfiguration",
      "s3:DeleteObjectVersion",
    ]
    resources = [
      aws_s3_bucket.state.arn,
      "${aws_s3_bucket.state.arn}/*",
    ]
  }

  # CI creates and updates the trail (see infra/), but can never silence it.
  # This also means `tofu destroy` cannot remove the trail — intentional.
  statement {
    sid    = "DenyAuditTampering"
    effect = "Deny"
    actions = [
      "cloudtrail:StopLogging",
      "cloudtrail:DeleteTrail",
      "cloudtrail:DeleteEventDataStore",
    ]
    resources = ["*"]
  }

  # CI must never be able to hand a human (or an attacker) console access, nor
  # touch the account's existence.
  statement {
    sid    = "DenyIdentityCenterAndAccountControl"
    effect = "Deny"
    actions = [
      "sso:*",
      "sso-directory:*",
      "identitystore:*",
      "organizations:LeaveOrganization",
      "account:CloseAccount",
      "iam:DeleteAccountPasswordPolicy",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_role_policy" "apply_guardrails" {
  name   = "guardrails"
  role   = aws_iam_role.apply.id
  policy = data.aws_iam_policy_document.apply_guardrails.json
}
