# ---------------------------------------------------------------------------
# Two buckets: uploaded documents, and the static SPA.
#
# Both are private. The account-wide public access block already forbids making
# them public; CloudFront reaches the site bucket through an Origin Access
# Control, which works against a private bucket and is why no bucket ACL or
# website-hosting configuration appears here.
# ---------------------------------------------------------------------------

# Account-regional namespace.
#
# Bucket names in this namespace are scoped to the account and region rather
# than being globally unique, so there is no competing for names with the rest
# of the internet and no need for a random suffix to win one.
#
# The suffix is NOT appended for you — S3 rejects the create with
# InvalidNamespaceHeader unless the name already ends in
# -<account-id>-<region>-an. Verified against the live API.
#
# All general-purpose bucket features work here; only table, vector and
# directory buckets are excluded from the namespace. No additional cost.
#
# The state and CloudTrail buckets in bootstrap/ stay on global names: existing
# buckets cannot be migrated into the namespace, only new ones created in it.
locals {
  bucket_ns_suffix = "${local.account_id}-${var.aws_region}-an"

  docs_bucket_name = "${var.project}-docs-${local.bucket_ns_suffix}"
  site_bucket_name = "${var.project}-site-${local.bucket_ns_suffix}"
}

# --- Uploaded documents ----------------------------------------------------

resource "aws_s3_bucket" "docs" {
  bucket           = local.docs_bucket_name
  bucket_namespace = "account-regional"

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_public_access_block" "docs" {
  bucket = aws_s3_bucket.docs.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "docs" {
  bucket = aws_s3_bucket.docs.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "docs" {
  bucket = aws_s3_bucket.docs.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_versioning" "docs" {
  bucket = aws_s3_bucket.docs.id

  versioning_configuration {
    status = "Enabled"
  }
}

# The browser PUTs directly to S3 with a presigned URL — the api Lambda must not
# proxy the upload, because Function URL payloads cap around 6 MB. That means
# the browser's PUT is cross-origin, so CORS is required for uploads to work at
# all.
#
# allowed_origins is the app origin only. A wildcard here would let any site
# make authenticated PUTs with a presigned URL the user's browser happens to
# hold.
resource "aws_s3_bucket_cors_configuration" "docs" {
  bucket = aws_s3_bucket.docs.id

  cors_rule {
    allowed_methods = ["PUT"]
    allowed_origins = ["https://${var.app_domain}"]
    allowed_headers = ["content-type"]
    expose_headers  = ["ETag"]
    max_age_seconds = 3000
  }
}

# Uploads are one-shot: a failed multipart leaves parts that bill as storage
# forever. Old versions are the same story on a bucket that is versioned only to
# survive a bad overwrite.
resource "aws_s3_bucket_lifecycle_configuration" "docs" {
  bucket = aws_s3_bucket.docs.id

  rule {
    id     = "abort-incomplete-multipart-uploads"
    status = "Enabled"

    filter {}

    abort_incomplete_multipart_upload {
      days_after_initiation = 1
    }
  }

  rule {
    id     = "expire-old-versions"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = 30
    }
  }

  depends_on = [aws_s3_bucket_versioning.docs]
}

# --- Static site -----------------------------------------------------------

resource "aws_s3_bucket" "site" {
  bucket           = local.site_bucket_name
  bucket_namespace = "account-regional"
}

resource "aws_s3_bucket_public_access_block" "site" {
  bucket = aws_s3_bucket.site.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "site" {
  bucket = aws_s3_bucket.site.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "site" {
  bucket = aws_s3_bucket.site.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

output "docs_bucket" {
  description = "Bucket receiving presigned PDF uploads."
  value       = aws_s3_bucket.docs.id
}

output "site_bucket" {
  description = "Bucket holding the built SPA; `aws s3 sync` target."
  value       = aws_s3_bucket.site.id
}
