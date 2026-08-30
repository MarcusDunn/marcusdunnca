# ---------------------------------------------------------------------------
# Edge delivery for the SPA.
#
# CloudFront is here for TLS on a custom domain and for a private origin, not
# for performance — one reader does not need a CDN. S3 cannot terminate TLS for
# study.aws.marcusdunn.ca on its own, and the alternative (a public
# website-hosting bucket) is forbidden by the account-wide public access block
# in bootstrap/, correctly.
#
# WHAT IS DELIBERATELY NOT HERE: a cache behaviour for the docs bucket.
#
# It would be easy to add and it would quietly destroy the security model.
# Uploaded PDFs are served through short-lived presigned GET URLs minted by the
# api Lambda after it has checked the caller's JWT. A CloudFront path to the
# docs bucket replaces that with a stable, unauthenticated, permanently valid
# URL for every document ever uploaded — and CloudFront would cache the
# contents at the edge on top. If a future change appears to need one, the
# answer is a signed-URL/signed-cookie behaviour or nothing.
# ---------------------------------------------------------------------------

# The site bucket is private and stays private. OAC is what lets CloudFront read
# it: the distribution signs its origin requests with SigV4 and the bucket
# policy below trusts that signature. "always" rather than "never" because a
# viewer-supplied Authorization header would otherwise be forwarded verbatim and
# break the signing.
resource "aws_cloudfront_origin_access_control" "site" {
  name                              = "${var.project}-site"
  description                       = "SigV4 access from CloudFront to the private site bucket."
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# Managed policy rather than a hand-rolled one: CachingOptimized compresses,
# honours origin cache headers, and forwards no cookies, headers or query
# strings — which is exactly right for immutable hashed assets and costs nothing
# to keep current.
data "aws_cloudfront_cache_policy" "optimized" {
  name = "Managed-CachingOptimized"
}

resource "aws_cloudfront_distribution" "site" {
  enabled             = true
  is_ipv6_enabled     = true
  comment             = "${var.project} reading trainer"
  default_root_object = "index.html"
  aliases             = [var.app_domain]

  # CloudFront is one of the few services the region lock in bootstrap/ cannot
  # constrain — it is global, so aws:RequestedRegion buys nothing here and edge
  # traffic is billed per region at rates that differ by a factor of three.
  # PriceClass_100 (North America and Europe) is therefore one of the only real
  # cost bounds available on this distribution. The reader is in Canada.
  price_class = "PriceClass_100"

  origin {
    origin_id = "site"

    # bucket_regional_domain_name, not bucket_domain_name: the global endpoint
    # redirects to the regional one for a non-us-east-1 bucket, and CloudFront
    # surfaces that as an opaque 307 loop.
    domain_name              = aws_s3_bucket.site.bucket_regional_domain_name
    origin_access_control_id = aws_cloudfront_origin_access_control.site.id
  }

  default_cache_behavior {
    target_origin_id = "site"

    # Read-only. Writes go to the api Function URL directly, never through here.
    allowed_methods = ["GET", "HEAD", "OPTIONS"]
    cached_methods  = ["GET", "HEAD"]

    viewer_protocol_policy = "redirect-to-https"
    compress               = true
    cache_policy_id        = data.aws_cloudfront_cache_policy.optimized.id
  }

  # Client-side routing. A deep link like /doc/abc123 is not an object in the
  # bucket, so S3 answers 404 — or 403, which is what a private bucket returns
  # instead when the key does not exist, because s3:ListBucket is not granted.
  # Both have to be rewritten to the app shell or every refresh on a sub-route
  # is a broken page.
  #
  # The cost of this is that genuinely missing assets also return 200 with HTML.
  # A short TTL keeps a transient origin problem from being cached as a
  # not-found for an hour.
  custom_error_response {
    error_code            = 403
    response_code         = 200
    response_page_path    = "/index.html"
    error_caching_min_ttl = 10
  }

  custom_error_response {
    error_code            = 404
    response_code         = 200
    response_page_path    = "/index.html"
    error_caching_min_ttl = 10
  }

  # Required block. No restriction: geo-blocking is not a security control and
  # would only get in the way of travelling.
  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    acm_certificate_arn = aws_acm_certificate.app.arn

    # sni-only. The alternative, a dedicated IP, is $600/month for compatibility
    # with browsers that predate 2013.
    ssl_support_method = "sni-only"

    # 2021 rather than the 2019 default: drops TLS 1.0/1.1 and the weaker 1.2
    # cipher suites outright. Nothing that needs to reach this site is that old.
    minimum_protocol_version = "TLSv1.2_2021"
  }

  # No logging_config. Access logs bill per GB stored in S3 forever, CloudTrail
  # in bootstrap/ already records the management-plane story, and there is no
  # analysis here that would read them.
}

# Confused-deputy protection. Without the SourceArn condition the policy trusts
# the CloudFront *service*, meaning anyone's distribution — including one
# created by a stranger pointing at this bucket — could read it. Access Analyzer
# in baseline.tf flags the unconditioned form, which is how this gets caught if
# it is ever loosened.
data "aws_iam_policy_document" "site_bucket" {
  statement {
    sid       = "AllowCloudFrontRead"
    effect    = "Allow"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.site.arn}/*"]

    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "AWS:SourceArn"
      values   = [aws_cloudfront_distribution.site.arn]
    }
  }
}

resource "aws_s3_bucket_policy" "site" {
  bucket = aws_s3_bucket.site.id
  policy = data.aws_iam_policy_document.site_bucket.json

  # block_public_policy evaluates the policy as it is written; applying before
  # the public access block exists is a window where a malformed policy would be
  # accepted.
  depends_on = [aws_s3_bucket_public_access_block.site]
}

output "cloudfront_distribution_domain_name" {
  description = "Target for the CNAME at Cloudflare (DNS-only / grey cloud — proxying it would break the SNI match against the ACM certificate)."
  value       = aws_cloudfront_distribution.site.domain_name
}

output "cloudfront_distribution_id" {
  description = "Invalidation target for the site deploy workflow."
  value       = aws_cloudfront_distribution.site.id
}
