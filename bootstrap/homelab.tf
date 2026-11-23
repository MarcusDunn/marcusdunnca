# ---------------------------------------------------------------------------
# The home cluster (github.com/MarcusDunn/homelab).
#
# Two things, both without long-lived credentials:
#
# 1. The zone home.marcusdunn.ca, for Let's Encrypt DNS-01. Only the
#    _acme-challenge TXT records go here. The services themselves resolve
#    through split-horizon DNS on the home VPN, so no private address is
#    published. Cloudflare delegates the zone (infra/dns.tf).
#
# 2. The cluster's service-account issuer, published at
#    https://oidc.home.marcusdunn.ca. STS fetches its discovery document and
#    JWKS from there, then trusts tokens that the cluster signs. A pod
#    exchanges its projected service-account token for credentials that last
#    an hour. The two files are public keys; the homelab repository publishes
#    them (scripts/publish-oidc.sh) whenever the signing key changes.
#
# 3. A role for the workflow of the homelab repository that applies that
#    repository's settings at GitHub (its github/ directory). The role does
#    one thing: read and write that configuration's state file in the state
#    bucket. GitHub signs a token for the job, as for this repository's CI.
#
# 4. A second role of the same kind, for the workflow that applies the
#    realm of Keycloak (the homelab repository's keycloak/ directory): that
#    configuration's state file only.
#
# This is bootstrap, not infra: it creates a hosted zone (denied to CI) and an
# identity provider plus a role that trusts it.
# ---------------------------------------------------------------------------

locals {
  homelab_zone        = "home.marcusdunn.ca"
  homelab_issuer_host = "oidc.${local.homelab_zone}"

  # The only identity that may assume the cert-manager role.
  homelab_cert_manager_sub = "system:serviceaccount:cert-manager:cert-manager"

  # The only job that may assume the settings role: a workflow of the
  # homelab repository that runs on its main branch. A pull request has
  # another subject. The numbers are the owner and the repository (see
  # var.github_owner_id): a renamed or re-created repository does not match.
  homelab_github_repo_id      = 1467036181
  homelab_github_settings_sub = "repo:${var.github_owner}@${var.github_owner_id}/homelab@${local.homelab_github_repo_id}:ref:refs/heads/main"

  homelab_github_state_key   = "homelab/github.tfstate"
  homelab_keycloak_state_key = "homelab/keycloak.tfstate"
}

# CloudFront accepts certificates from us-east-1 only: aws.us_east_1 is the
# provider in cost.tf.

resource "aws_route53_zone" "homelab" {
  name    = local.homelab_zone
  comment = "Home cluster: DNS-01 challenges and the service-account issuer."
}

# --- The issuer: a private bucket behind CloudFront ------------------------

resource "aws_s3_bucket" "homelab_oidc" {
  bucket = "${var.project}-homelab-oidc-${data.aws_caller_identity.current.account_id}"
}

resource "aws_s3_bucket_public_access_block" "homelab_oidc" {
  bucket = aws_s3_bucket.homelab_oidc.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "homelab_oidc" {
  bucket = aws_s3_bucket.homelab_oidc.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "homelab_oidc" {
  bucket = aws_s3_bucket.homelab_oidc.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_cloudfront_origin_access_control" "homelab_oidc" {
  name                              = "${var.project}-homelab-oidc"
  description                       = "CloudFront reads the homelab issuer bucket."
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

data "aws_iam_policy_document" "homelab_oidc_bucket" {
  statement {
    sid       = "CloudFrontReadsObjects"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.homelab_oidc.arn}/*"]

    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "AWS:SourceArn"
      values   = [aws_cloudfront_distribution.homelab_oidc.arn]
    }
  }

  statement {
    sid     = "DenyInsecureTransport"
    effect  = "Deny"
    actions = ["s3:*"]
    resources = [
      aws_s3_bucket.homelab_oidc.arn,
      "${aws_s3_bucket.homelab_oidc.arn}/*",
    ]

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }
}

resource "aws_s3_bucket_policy" "homelab_oidc" {
  bucket = aws_s3_bucket.homelab_oidc.id
  policy = data.aws_iam_policy_document.homelab_oidc_bucket.json

  depends_on = [aws_s3_bucket_public_access_block.homelab_oidc]
}

resource "aws_acm_certificate" "homelab_oidc" {
  provider = aws.us_east_1

  domain_name       = local.homelab_issuer_host
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

# ACM renews the certificate while this record exists.
resource "aws_route53_record" "homelab_oidc_validation" {
  for_each = {
    for o in aws_acm_certificate.homelab_oidc.domain_validation_options : o.domain_name => o
  }

  zone_id         = aws_route53_zone.homelab.zone_id
  name            = each.value.resource_record_name
  type            = each.value.resource_record_type
  records         = [each.value.resource_record_value]
  ttl             = 300
  allow_overwrite = true
}

resource "aws_acm_certificate_validation" "homelab_oidc" {
  provider = aws.us_east_1

  certificate_arn         = aws_acm_certificate.homelab_oidc.arn
  validation_record_fqdns = [for r in aws_route53_record.homelab_oidc_validation : r.fqdn]
}

resource "aws_cloudfront_distribution" "homelab_oidc" {
  enabled         = true
  comment         = "Homelab service-account issuer (OIDC discovery and JWKS)."
  aliases         = [local.homelab_issuer_host]
  price_class     = "PriceClass_100"
  http_version    = "http2and3"
  is_ipv6_enabled = true

  origin {
    origin_id                = "homelab-oidc"
    domain_name              = aws_s3_bucket.homelab_oidc.bucket_regional_domain_name
    origin_access_control_id = aws_cloudfront_origin_access_control.homelab_oidc.id
  }

  default_cache_behavior {
    target_origin_id       = "homelab-oidc"
    viewer_protocol_policy = "https-only"
    allowed_methods        = ["GET", "HEAD"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true
    # Managed policy "CachingOptimized".
    cache_policy_id = "658327ea-f89d-4fab-a63d-7e88639e58f6"
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    acm_certificate_arn      = aws_acm_certificate_validation.homelab_oidc.certificate_arn
    ssl_support_method       = "sni-only"
    minimum_protocol_version = "TLSv1.2_2021"
  }
}

resource "aws_route53_record" "homelab_oidc" {
  for_each = toset(["A", "AAAA"])

  zone_id = aws_route53_zone.homelab.zone_id
  name    = local.homelab_issuer_host
  type    = each.key

  alias {
    name                   = aws_cloudfront_distribution.homelab_oidc.domain_name
    zone_id                = aws_cloudfront_distribution.homelab_oidc.hosted_zone_id
    evaluate_target_health = false
  }
}

# --- Trust in the cluster's service-account tokens -------------------------

# AWS validates this issuer against its own trust store; no thumbprint. IAM
# connects to the issuer when it creates the provider, so the first apply
# goes in two steps: everything except this provider and the role, then
# scripts/publish-oidc.sh (homelab repository), then the rest.
resource "aws_iam_openid_connect_provider" "homelab" {
  url            = "https://${local.homelab_issuer_host}"
  client_id_list = ["sts.amazonaws.com"]
}

data "aws_iam_policy_document" "homelab_cert_manager_trust" {
  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.homelab.arn]
    }

    condition {
      test     = "StringEquals"
      variable = "${local.homelab_issuer_host}:sub"
      values   = [local.homelab_cert_manager_sub]
    }

    condition {
      test     = "StringEquals"
      variable = "${local.homelab_issuer_host}:aud"
      values   = ["sts.amazonaws.com"]
    }
  }
}

# cert-manager may only write and remove the TXT record of the wildcard
# certificate's challenge, in this one zone.
data "aws_iam_policy_document" "homelab_cert_manager" {
  statement {
    sid       = "ReadChangeStatus"
    actions   = ["route53:GetChange"]
    resources = ["arn:${data.aws_partition.current.partition}:route53:::change/*"]
  }

  statement {
    sid       = "ReadZone"
    actions   = ["route53:ListResourceRecordSets"]
    resources = [aws_route53_zone.homelab.arn]
  }

  statement {
    sid       = "ChallengeRecordOnly"
    actions   = ["route53:ChangeResourceRecordSets"]
    resources = [aws_route53_zone.homelab.arn]

    condition {
      test     = "ForAllValues:StringEquals"
      variable = "route53:ChangeResourceRecordSetsRecordTypes"
      values   = ["TXT"]
    }

    condition {
      test     = "ForAllValues:StringEquals"
      variable = "route53:ChangeResourceRecordSetsNormalizedRecordNames"
      values   = ["_acme-challenge.${local.homelab_zone}"]
    }

    condition {
      test     = "ForAllValues:StringEquals"
      variable = "route53:ChangeResourceRecordSetsActions"
      values   = ["UPSERT", "DELETE"]
    }
  }
}

resource "aws_iam_role" "homelab_cert_manager" {
  name                 = "homelab-cert-manager"
  path                 = "/homelab/"
  description          = "cert-manager in the home cluster: DNS-01 in home.marcusdunn.ca."
  assume_role_policy   = data.aws_iam_policy_document.homelab_cert_manager_trust.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy" "homelab_cert_manager" {
  name   = "dns01-challenge"
  role   = aws_iam_role.homelab_cert_manager.id
  policy = data.aws_iam_policy_document.homelab_cert_manager.json
}

# --- The settings of the homelab repository at GitHub ----------------------

data "aws_iam_policy_document" "homelab_github_settings_trust" {
  statement {
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
      values   = [local.homelab_github_settings_sub]
    }
  }
}

# One state file and its lock. The state holds no secret: the settings of a
# repository, which the GitHub API also returns.
data "aws_iam_policy_document" "homelab_github_settings" {
  statement {
    sid       = "FindState"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.state.arn]

    condition {
      test     = "StringEquals"
      variable = "s3:prefix"
      values   = [local.homelab_github_state_key, "${local.homelab_github_state_key}.tflock"]
    }
  }

  statement {
    sid       = "ReadWriteState"
    actions   = ["s3:GetObject", "s3:PutObject"]
    resources = ["${aws_s3_bucket.state.arn}/${local.homelab_github_state_key}"]
  }

  statement {
    sid       = "Lock"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.state.arn}/${local.homelab_github_state_key}.tflock"]
  }
}

resource "aws_iam_role" "homelab_github_settings" {
  name                 = "homelab-github-settings"
  path                 = "/homelab/"
  description          = "The homelab repository's workflow that applies its GitHub settings: its state file only."
  assume_role_policy   = data.aws_iam_policy_document.homelab_github_settings_trust.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy" "homelab_github_settings" {
  name   = "state"
  role   = aws_iam_role.homelab_github_settings.id
  policy = data.aws_iam_policy_document.homelab_github_settings.json
}

# --- The realm of Keycloak in the home cluster ------------------------------

# The same job identity as above: a workflow of the homelab repository on
# its main branch. One state file and its lock. The state holds no secret:
# the realm's clients are public, or get their secret another way.
data "aws_iam_policy_document" "homelab_keycloak" {
  statement {
    sid       = "FindState"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.state.arn]

    condition {
      test     = "StringEquals"
      variable = "s3:prefix"
      values   = [local.homelab_keycloak_state_key, "${local.homelab_keycloak_state_key}.tflock"]
    }
  }

  statement {
    sid       = "ReadWriteState"
    actions   = ["s3:GetObject", "s3:PutObject"]
    resources = ["${aws_s3_bucket.state.arn}/${local.homelab_keycloak_state_key}"]
  }

  statement {
    sid       = "Lock"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.state.arn}/${local.homelab_keycloak_state_key}.tflock"]
  }
}

resource "aws_iam_role" "homelab_keycloak" {
  name                 = "homelab-keycloak"
  path                 = "/homelab/"
  description          = "The homelab repository's workflow that applies the realm of Keycloak: its state file only."
  assume_role_policy   = data.aws_iam_policy_document.homelab_github_settings_trust.json
  max_session_duration = 3600
}

resource "aws_iam_role_policy" "homelab_keycloak" {
  name   = "state"
  role   = aws_iam_role.homelab_keycloak.id
  policy = data.aws_iam_policy_document.homelab_keycloak.json
}

output "homelab_zone_id" {
  description = "Route53 zone ID of home.marcusdunn.ca (cert-manager's hostedZoneID)."
  value       = aws_route53_zone.homelab.zone_id
}

output "homelab_name_servers" {
  description = "Name servers that Cloudflare delegates home.marcusdunn.ca to (infra/dns.tf)."
  value       = aws_route53_zone.homelab.name_servers
}

output "homelab_issuer_url" {
  description = "The cluster's service-account issuer (kube-apiserver --service-account-issuer)."
  value       = "https://${local.homelab_issuer_host}"
}

output "homelab_oidc_bucket" {
  description = "Bucket that holds the issuer's discovery document and JWKS."
  value       = aws_s3_bucket.homelab_oidc.id
}

output "homelab_oidc_distribution_id" {
  description = "CloudFront distribution in front of the issuer bucket (for invalidations)."
  value       = aws_cloudfront_distribution.homelab_oidc.id
}

output "homelab_cert_manager_role_arn" {
  description = "Role that cert-manager assumes with its service-account token."
  value       = aws_iam_role.homelab_cert_manager.arn
}

output "homelab_github_settings_role_arn" {
  description = "The role in .github/workflows/github-settings.yaml of the homelab repository."
  value       = aws_iam_role.homelab_github_settings.arn
}

output "homelab_keycloak_role_arn" {
  description = "The role in .github/workflows/keycloak.yaml of the homelab repository."
  value       = aws_iam_role.homelab_keycloak.arn
}
