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

# ---------------------------------------------------------------------------
# Security response headers for the SPA.
#
# The session token lives in localStorage — a reasoned trade, documented in
# web/src/lib/auth.ts — which makes an XSS a thirty-day credential theft. The
# app renders a great deal of model-generated and reader-typed text, and React's
# escaping is the only thing between that text and the DOM. A Content Security
# Policy is the second control, and at CloudFront it costs nothing.
#
# The CSP ships REPORT-ONLY. Everything in it was checked against the built
# bundle (Vite emits no inline script or style for this configuration, and the
# app has none) but not against a browser rendering the PDF <embed> or issuing
# the presigned PUT, and a wrong directive there breaks uploads or the reader
# silently. Run the app with devtools open, confirm the console shows no CSP
# reports through an upload and a quiz, then promote it: move local.site_csp
# into security_headers_config as content_security_policy and delete the
# custom header. The other four headers are enforced now — none of them can
# break anything this app does.
# ---------------------------------------------------------------------------

locals {
  # The one origin the app touches besides its own: the documents bucket, for
  # the presigned PUT (connect-src) and the presigned GET behind the <embed>
  # (object-src; frame-src as well, for browsers that host their PDF viewer in
  # a frame). Presigned URLs from the Rust SDK use the virtual-hosted regional
  # form, which is what bucket_regional_domain_name is — if the report-only run
  # shows connect-src or object-src violations, this is the line to look at.
  docs_origin = "https://${aws_s3_bucket.docs.bucket_regional_domain_name}"

  site_csp = join("; ", [
    "default-src 'self'",
    "script-src 'self'",
    "style-src 'self'",
    "img-src 'self' data:",
    "connect-src 'self' ${local.docs_origin}",
    "object-src ${local.docs_origin}",
    "frame-src ${local.docs_origin}",
    "frame-ancestors 'none'",
    "base-uri 'none'",
    "form-action 'self'",
  ])
}

resource "aws_cloudfront_response_headers_policy" "site" {
  name    = "${var.project}-site"
  comment = "Security headers for the SPA. The CSP is report-only until it has been verified in a browser."

  security_headers_config {
    # A year, no subdomains and no preload: the apex and its other subdomains
    # are Cloudflare's to promise for, not this distribution's.
    strict_transport_security {
      access_control_max_age_sec = 31536000
      include_subdomains         = false
      preload                    = false
      override                   = true
    }

    content_type_options {
      override = true
    }

    # Nothing frames this app. Passkey prompts already refuse a cross-origin
    # frame; this covers the rest of the UI.
    frame_options {
      frame_option = "DENY"
      override     = true
    }

    referrer_policy {
      referrer_policy = "strict-origin-when-cross-origin"
      override        = true
    }
  }

  custom_headers_config {
    items {
      header   = "Content-Security-Policy-Report-Only"
      value    = local.site_csp
      override = true
    }
  }
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

  origin {
    origin_id = "api"
    # The gateway host, without scheme. No Origin Access Control: an HTTP API is
    # an ordinary HTTPS origin, so CloudFront forwards headers through untouched
    # rather than overwriting Authorization with its own SigV4 signature.
    domain_name = replace(aws_apigatewayv2_api.api.api_endpoint, "https://", "")

    custom_origin_config {
      http_port              = 80
      https_port             = 443
      origin_protocol_policy = "https-only"
      origin_ssl_protocols   = ["TLSv1.2"]
    }
  }

  ordered_cache_behavior {
    path_pattern           = "/api/*"
    target_origin_id       = "api"
    viewer_protocol_policy = "https-only"
    allowed_methods        = ["GET", "HEAD", "OPTIONS", "PUT", "POST", "PATCH", "DELETE"]
    cached_methods         = ["GET", "HEAD"]

    cache_policy_id          = data.aws_cloudfront_cache_policy.caching_disabled.id
    origin_request_policy_id = data.aws_cloudfront_origin_request_policy.all_viewer_except_host.id

    function_association {
      event_type   = "viewer-request"
      function_arn = aws_cloudfront_function.strip_api_prefix.arn
    }
  }

  default_cache_behavior {
    function_association {
      event_type   = "viewer-request"
      function_arn = aws_cloudfront_function.spa_rewrite.arn
    }

    target_origin_id = "site"

    # Read-only. Writes go to the api Function URL directly, never through here.
    allowed_methods = ["GET", "HEAD", "OPTIONS"]
    cached_methods  = ["GET", "HEAD"]

    viewer_protocol_policy = "redirect-to-https"
    compress               = true
    cache_policy_id        = data.aws_cloudfront_cache_policy.optimized.id

    # On the site behaviour only. The api sets its own headers in the handler,
    # and HSTS is a per-host promise the browser learns from the first page it
    # loads, which is always this one.
    response_headers_policy_id = aws_cloudfront_response_headers_policy.site.id
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
  # NO custom_error_response, deliberately.
  #
  # It is configured per-distribution, not per-behaviour, so mapping 403/404 to
  # index.html rewrote the API's errors too: a 403 from the Function URL came
  # back as index.html with a 200 and `server: AmazonS3`. That masked a real
  # SigV4 failure completely — the handler was never invoked and there was
  # nothing in its logs, while the response looked like a success.
  #
  # SPA deep links are handled by the rewrite function on the default behaviour
  # instead, which only ever runs for the site origin.

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

# ---------------------------------------------------------------------------
# The api, served from the same origin as the SPA.
#
# Routing /api/* through this distribution rather than calling the Function URL
# directly removes CORS from the picture entirely — a same-origin request has no
# preflight, no Access-Control-Allow-Origin, and therefore none of the failure
# modes that cost most of an evening here: a duplicate ACAO header from two
# layers both configuring CORS, a preflight cached for an hour against a config
# that had since changed, and a custom header refused by a CORS block that
# answers preflight without ever invoking the handler.
#
# It also closes a real exposure. The Function URL was AUTH_TYPE=NONE and
# publicly invocable by anyone who knew it; with account concurrency capped at
# 10, that is a trivially available exhaustion vector. Behind an Origin Access
# Control with AWS_IAM, only this distribution can invoke it.
# ---------------------------------------------------------------------------

# CloudFront forwards the full path, so /api/docs would arrive as /api/docs and
# match no route. Stripping the prefix at the edge keeps the handler's routing
# table identical whether it is reached directly or through the distribution —
# the alternative, teaching the handler about a prefix that only exists in one
# deployment topology, bakes the CDN into the application.
resource "aws_cloudfront_function" "strip_api_prefix" {
  name    = "${var.project}-strip-api-prefix"
  runtime = "cloudfront-js-2.0"
  publish = true
  comment = "Removes the /api prefix before the request reaches the Function URL."

  code = <<-JS
    function handler(event) {
      var request = event.request;
      request.uri = request.uri.replace(/^\/api/, '');
      if (request.uri === '') {
        request.uri = '/';
      }
      return request;
    }
  JS
}

# Caching disabled, and Authorization forwarded. CloudFront strips Authorization
# by default — the single most common way an API behind a distribution fails,
# because every request simply arrives unauthenticated.
data "aws_cloudfront_cache_policy" "caching_disabled" {
  name = "Managed-CachingDisabled"
}

data "aws_cloudfront_origin_request_policy" "all_viewer_except_host" {
  name = "Managed-AllViewerExceptHostHeader"
}


output "api_base_url" {
  description = "Same-origin path the SPA calls. Not the Function URL, which is no longer publicly invocable."
  value       = "https://${var.app_domain}/api"
}

# Extensionless paths are client-side routes and must serve the SPA shell;
# anything with an extension is a real asset and must 404 honestly if missing.
#
# Doing this as a rewrite rather than an error mapping keeps it scoped to the
# default behaviour, so it cannot touch /api/* — see the note above.
resource "aws_cloudfront_function" "spa_rewrite" {
  name    = "${var.project}-spa-rewrite"
  runtime = "cloudfront-js-2.0"
  publish = true
  comment = "Serves index.html for client-side routes without masking asset or API errors."

  code = <<-JS
    function handler(event) {
      var request = event.request;
      var uri = request.uri;
      if (uri === '/' || uri.indexOf('.') !== -1) {
        return request;
      }
      request.uri = '/index.html';
      return request;
    }
  JS
}
