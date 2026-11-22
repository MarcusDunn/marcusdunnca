# ---------------------------------------------------------------------------
# DNS records at Cloudflare.
#
# marcusdunn.ca is registered at Cloudflare, and Cloudflare is its DNS host.
# There is no Route53 zone. This file manages only the records below. Other
# records in the zone (mail, and the records for other sites) stay manual.
#
# The provider reads CLOUDFLARE_API_TOKEN from the environment. Cloudflare does
# not accept GitHub OIDC, so the tokens are in SSM, behind the AWS roles that
# do. The workflows read them with the AWS CLI, never with a data source, so no
# value goes into state.
#   - tofu plan (pull requests): /ci/cloudflare/dns-read, DNS Read only. Any
#     pull request gets the plan role, so this token must not edit.
#   - tofu apply (main): /ci/cloudflare/dns-edit, DNS Write. Only the
#     `production` environment can assume the apply role.
# bootstrap/iam.tf gives each role its one parameter. CI cannot write either.
# scripts/cloudflare-setup.sh makes and rotates the tokens.
#
# To plan from a workstation, set CLOUDFLARE_API_TOKEN to the read token:
#   export CLOUDFLARE_API_TOKEN=$(aws ssm get-parameter --name /ci/cloudflare/dns-read \
#     --with-decryption --query Parameter.Value --output text)
#
# Both tokens are limited to the marcusdunn.ca zone. Cloudflare cannot limit a
# token to one record, so the edit token can change any record in the zone.
#
# Every record is DNS only (proxied = false):
#   - CloudFront must see the TLS SNI of its own ACM certificate.
#   - ACM validation needs the raw CNAME.
#   - The Cloudflare proxy does not carry the UDP traffic of WireGuard.
# ---------------------------------------------------------------------------

locals {
  # ttl = 1 is "automatic" at Cloudflare.
  cloudflare_auto_ttl = 1
  dns_record_comment  = "Managed by OpenTofu: marcusdunnca/infra/dns.tf"
}

resource "cloudflare_dns_record" "app" {
  zone_id = var.cloudflare_zone_id
  name    = var.app_domain
  type    = "CNAME"
  content = aws_cloudfront_distribution.site.domain_name
  ttl     = local.cloudflare_auto_ttl
  proxied = false
  comment = local.dns_record_comment
}

# ACM renews the certificate automatically while this record exists. If it is
# removed, renewal fails and the site loses TLS when the certificate expires.
resource "cloudflare_dns_record" "acm_validation" {
  for_each = {
    for o in aws_acm_certificate.app.domain_validation_options : o.domain_name => o
  }

  zone_id = var.cloudflare_zone_id
  # ACM gives fully qualified names with a trailing dot. Cloudflare keeps them
  # without the dot, so the dot would show as a change on each plan.
  name    = trimsuffix(each.value.resource_record_name, ".")
  type    = each.value.resource_record_type
  content = trimsuffix(each.value.resource_record_value, ".")
  ttl     = local.cloudflare_auto_ttl
  proxied = false
  comment = local.dns_record_comment
}

# The WireGuard server on the home cluster node (marcusd-home, Talos). The
# router forwards UDP 51820 to it. Its configuration is in the homelab
# repository, in talos/talconfig.yaml.
resource "cloudflare_dns_record" "vpn" {
  zone_id = var.cloudflare_zone_id
  name    = var.vpn_domain
  type    = "A"
  content = var.home_public_ipv4
  ttl     = local.cloudflare_auto_ttl
  proxied = false
  comment = local.dns_record_comment
}

# home.marcusdunn.ca is a Route53 zone (bootstrap/homelab.tf): the home
# cluster's Let's Encrypt DNS-01 challenges and its service-account issuer.
# These records delegate it. The name servers are the zone's
# (tofu -chdir=bootstrap output homelab_name_servers); a hosted zone keeps
# them for its life.
resource "cloudflare_dns_record" "homelab_delegation" {
  for_each = toset(var.homelab_name_servers)

  zone_id = var.cloudflare_zone_id
  name    = var.homelab_zone
  type    = "NS"
  content = each.value
  ttl     = local.cloudflare_auto_ttl
  proxied = false
  comment = local.dns_record_comment
}

# The app and ACM records were made by hand before this file existed. These
# blocks adopt them into state. Without them, the first apply would try to make
# duplicate records. After the first apply they do nothing, and you can remove
# them.
#
# The import ID is "<zone_id>/<record_id>".
import {
  to = cloudflare_dns_record.app
  id = "${var.cloudflare_zone_id}/91ddf800db9f60680466ad51fae897de"
}

import {
  to = cloudflare_dns_record.acm_validation[var.app_domain]
  id = "${var.cloudflare_zone_id}/43095fc52f8fd7c95eaa3770f075e1f0"
}

output "vpn_endpoint" {
  description = "Endpoint for WireGuard peers."
  value       = "${cloudflare_dns_record.vpn.name}:51820"
}
