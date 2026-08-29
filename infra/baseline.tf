data "aws_caller_identity" "current" {}
data "aws_partition" "current" {}

locals {
  account_id = data.aws_caller_identity.current.account_id
  partition  = data.aws_partition.current.partition
}

# ---------------------------------------------------------------------------
# Account-wide guardrails. Every resource here is free; together they close the
# defaults AWS ships that are wrong for a security-first account.
# ---------------------------------------------------------------------------

resource "aws_iam_account_alias" "this" {
  account_alias = var.account_alias
}

# Overrides any per-bucket setting anywhere in the account. Even a future bucket
# created by hand with a public ACL stays private.
resource "aws_s3_account_public_access_block" "this" {
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

# New EBS volumes are unencrypted by default. Flip that account-wide so it is
# impossible to forget when the app lands.
resource "aws_ebs_encryption_by_default" "this" {
  enabled = true
}

# Applies to IAM users. There are none today and the guardrails forbid creating
# them, but this is the correct floor if that ever changes under an emergency.
resource "aws_iam_account_password_policy" "this" {
  minimum_password_length        = 24
  require_lowercase_characters   = true
  require_uppercase_characters   = true
  require_numbers                = true
  require_symbols                = true
  allow_users_to_change_password = true
  max_password_age               = 365
  password_reuse_prevention      = 24
}

# Flags any resource policy that grants access outside this account — the single
# highest-signal, zero-cost detection available.
resource "aws_accessanalyzer_analyzer" "account" {
  analyzer_name = "${var.project}-external-access"
  type          = "ACCOUNT"
}

resource "aws_account_alternate_contact" "security" {
  count = var.security_contact == null ? 0 : 1

  alternate_contact_type = "SECURITY"
  name                   = var.security_contact.name
  title                  = var.security_contact.title
  email_address          = var.security_contact.email_address
  phone_number           = var.security_contact.phone_number
}
