data "aws_caller_identity" "current" {}
data "aws_partition" "current" {}

locals {
  account_id = data.aws_caller_identity.current.account_id
  partition  = data.aws_partition.current.partition

  # Globally unique without needing a random suffix, and self-documenting.
  state_bucket = "${var.project}-tfstate-${data.aws_caller_identity.current.account_id}"
}

# ---------------------------------------------------------------------------
# Optional customer-managed key for state at rest. Off by default (~$1/mo).
# ---------------------------------------------------------------------------

resource "aws_kms_key" "state" {
  count = var.state_bucket_use_cmk ? 1 : 0

  description             = "${var.project} OpenTofu state encryption"
  enable_key_rotation     = true
  deletion_window_in_days = 30

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_kms_alias" "state" {
  count = var.state_bucket_use_cmk ? 1 : 0

  name          = "alias/${var.project}-tfstate"
  target_key_id = aws_kms_key.state[0].key_id
}

# ---------------------------------------------------------------------------
# State bucket
# ---------------------------------------------------------------------------

resource "aws_s3_bucket" "state" {
  bucket = local.state_bucket

  # This bucket holds the state that describes the whole account, including its
  # own definition. Losing it is the single worst outcome in this repo, so refuse
  # to destroy it even if someone runs `tofu destroy` in here.
  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_versioning" "state" {
  bucket = aws_s3_bucket.state.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  bucket = aws_s3_bucket.state.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = var.state_bucket_use_cmk ? "aws:kms" : "AES256"
      kms_master_key_id = var.state_bucket_use_cmk ? aws_kms_key.state[0].arn : null
    }
    # Bucket keys cut KMS request costs substantially; meaningless for SSE-S3.
    bucket_key_enabled = var.state_bucket_use_cmk
  }
}

resource "aws_s3_bucket_public_access_block" "state" {
  bucket = aws_s3_bucket.state.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "state" {
  bucket = aws_s3_bucket.state.id

  rule {
    # Disables ACLs entirely — access is governed by policy alone.
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "state" {
  bucket = aws_s3_bucket.state.id

  # Versioning is what makes state recoverable, but unbounded versions are an
  # unbounded bill. 90 days is far longer than any realistic recovery window.
  rule {
    id     = "expire-noncurrent-state-versions"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = var.state_noncurrent_version_retention_days
    }
  }

  rule {
    id     = "abort-incomplete-multipart-uploads"
    status = "Enabled"

    filter {}

    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }

  depends_on = [aws_s3_bucket_versioning.state]
}

data "aws_iam_policy_document" "state_bucket" {
  statement {
    sid    = "DenyInsecureTransport"
    effect = "Deny"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions   = ["s3:*"]
    resources = [aws_s3_bucket.state.arn, "${aws_s3_bucket.state.arn}/*"]

    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }

  statement {
    sid    = "DenyOutdatedTLS"
    effect = "Deny"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions   = ["s3:*"]
    resources = [aws_s3_bucket.state.arn, "${aws_s3_bucket.state.arn}/*"]

    condition {
      test     = "NumericLessThan"
      variable = "s3:TlsVersion"
      values   = ["1.2"]
    }
  }

  # Belt-and-braces against a future bucket policy or cross-account grant ever
  # exposing state outside this account. Only principals in this account may
  # touch it, regardless of what any other policy says.
  statement {
    sid    = "DenyPrincipalsOutsideThisAccount"
    effect = "Deny"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions   = ["s3:*"]
    resources = [aws_s3_bucket.state.arn, "${aws_s3_bucket.state.arn}/*"]

    condition {
      test     = "StringNotEquals"
      variable = "aws:PrincipalAccount"
      values   = [local.account_id]
    }
  }
}

resource "aws_s3_bucket_policy" "state" {
  bucket = aws_s3_bucket.state.id
  policy = data.aws_iam_policy_document.state_bucket.json

  # Applying a restrictive policy before public access is blocked would be a
  # brief window of wrong ordering; make the dependency explicit.
  depends_on = [aws_s3_bucket_public_access_block.state]
}
