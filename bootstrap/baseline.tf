# ---------------------------------------------------------------------------
# The account's security floor.
#
# These live in bootstrap rather than infra deliberately. Under a
# compromised-CI threat model, the controls that would let an attacker weaken
# the account or conceal their activity must not be writable by CI. The apply
# role has no permission to change any of them — see the guardrail policy in
# iam.tf, which denies these actions outright.
#
# Every resource here is free.
# ---------------------------------------------------------------------------

# Friendly console sign-in URL. Owned here rather than in infra/ because
# account aliases live in a global, first-come namespace: releasing this one
# lets anyone claim `marcusdunnca` and stand up a convincing sign-in page at the
# URL you have bookmarked, permanently.
resource "aws_iam_account_alias" "this" {
  account_alias = var.account_alias
}

# Overrides any per-bucket setting anywhere in the account. Even a bucket
# created by hand with a public ACL stays private.
resource "aws_s3_account_public_access_block" "this" {
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

# New EBS volumes are unencrypted by default. Flip that account-wide so it
# cannot be forgotten when the application lands.
resource "aws_ebs_encryption_by_default" "this" {
  enabled = true
}

# Applies to IAM users. There are none, and both CI roles are denied the ability
# to create any — but this is the correct floor if that ever changes by hand.
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

# AWS uses this to reach a human about abuse reports and compromised-credential
# notices rather than falling back to the root mailbox.
resource "aws_account_alternate_contact" "security" {
  count = var.security_contact == null ? 0 : 1

  alternate_contact_type = "SECURITY"
  name                   = var.security_contact.name
  title                  = var.security_contact.title
  email_address          = var.security_contact.email_address
  phone_number           = var.security_contact.phone_number
}
