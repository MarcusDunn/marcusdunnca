# ---------------------------------------------------------------------------
# TLS certificate for the application domain.
#
# Requested first, deliberately, because DNS validation needs a human to add a
# record at Cloudflare and CloudFront will not accept an unissued certificate.
# Getting the request in early lets that wait happen in parallel with the rest
# of the build rather than blocking at the end.
#
# Note there is NO aws_acm_certificate_validation resource here. That resource
# blocks apply until the certificate is issued, which would hang CI for as long
# as the DNS record is missing. The certificate sits in PENDING_VALIDATION
# harmlessly; CloudFront simply cannot be created until it goes ISSUED.
#
# Free — ACM public certificates cost nothing, including renewal, which is
# automatic for as long as the validation record stays in place.
# ---------------------------------------------------------------------------

resource "aws_acm_certificate" "app" {
  provider = aws.us_east_1

  domain_name       = var.app_domain
  validation_method = "DNS"

  lifecycle {
    # ACM issues a replacement before the old one is released, so the
    # distribution is never left without a certificate mid-apply.
    create_before_destroy = true
  }
}

# What Marcus needs to create at Cloudflare. Surfaced as an output rather than
# buried in state so `tofu output` answers "what do I paste where".
output "acm_validation_record" {
  description = "DNS record to create at Cloudflare (DNS-only / grey cloud) to validate the certificate."
  value = {
    for o in aws_acm_certificate.app.domain_validation_options :
    o.domain_name => {
      name  = o.resource_record_name
      type  = o.resource_record_type
      value = o.resource_record_value
    }
  }
}

output "acm_certificate_arn" {
  description = "Certificate ARN, consumed by the CloudFront distribution once ISSUED."
  value       = aws_acm_certificate.app.arn
}

output "acm_certificate_status" {
  description = "PENDING_VALIDATION until the Cloudflare record exists and ACM has seen it."
  value       = aws_acm_certificate.app.status
}
