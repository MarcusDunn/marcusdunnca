data "aws_caller_identity" "current" {}

locals {
  account_id = data.aws_caller_identity.current.account_id
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

# Friendly console sign-in URL. Cosmetic; safe for CI to own.
resource "aws_iam_account_alias" "this" {
  account_alias = var.account_alias
}

# Flags any resource policy granting access outside this account. Free, and the
# highest-signal detection available at zero cost.
#
# CI may create and tag this analyzer but is denied access-analyzer:DeleteAnalyzer
# by the guardrails, so it cannot switch off the thing watching it.
resource "aws_accessanalyzer_analyzer" "account" {
  analyzer_name = "${var.project}-external-access"
  type          = "ACCOUNT"
}
