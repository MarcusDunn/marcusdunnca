data "aws_caller_identity" "current" {}

# Hardcoding "aws" would work today and break silently in any other partition.
# The permissions boundary ARN below has to match byte-for-byte or role creation
# is denied, so it is worth deriving rather than assuming.
data "aws_partition" "current" {}

locals {
  account_id = data.aws_caller_identity.current.account_id
  partition  = data.aws_partition.current.partition

  # Every role this module creates MUST carry this, and the value is not
  # negotiable: bootstrap's DenyRoleWorkWithoutBoundary matches on the exact ARN
  # and the apply role loses iam:CreateRole without it. A typo here does not
  # degrade gracefully, it fails the apply.
  permissions_boundary_arn = "arn:${local.partition}:iam::${local.account_id}:policy/${var.project}-ci-permissions-boundary"
}

# ---------------------------------------------------------------------------
# Resources CI is trusted to manage.
#
# The account's security floor — CloudTrail, the account-wide public access
# block, the password policy, EBS encryption defaults, alternate contacts —
# deliberately does NOT live here. It is in bootstrap/, applied by a human, and
# the apply role is denied permission to change any of it. A compromised
# pipeline must not be able to weaken the account or hide what it did.
#
# What remains here is operational, and is where application infrastructure will
# go. Every addition needs a matching entry in the apply role's allowlist in
# bootstrap/iam.tf — that expansion is intended to be a deliberate, reviewed act.
# ---------------------------------------------------------------------------

# NOTE: the account alias moved to bootstrap/. Aliases are a global, first-come
# namespace, so the ability to delete one is the ability to let a stranger claim
# your console sign-in URL for good. CI is now denied both alias write actions.

# Flags any resource policy granting access outside this account. Free, and the
# highest-signal detection available at zero cost.
#
# CI may create and tag this analyzer but is denied access-analyzer:DeleteAnalyzer
# by the guardrails, so it cannot switch off the thing watching it.
resource "aws_accessanalyzer_analyzer" "account" {
  analyzer_name = "${var.project}-external-access"
  type          = "ACCOUNT"
}
