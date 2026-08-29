# ---------------------------------------------------------------------------
# GitHub Actions OIDC trust anchor.
#
# This is what removes long-lived AWS credentials from the picture entirely:
# GitHub mints a short-lived JWT per job, AWS validates it against this provider,
# and STS hands back credentials that expire in an hour. There is no access key
# to leak, rotate, or find in a compromised repo.
# ---------------------------------------------------------------------------

data "tls_certificate" "github_actions" {
  url = "https://token.actions.githubusercontent.com/.well-known/openid-configuration"
}

resource "aws_iam_openid_connect_provider" "github_actions" {
  url            = "https://token.actions.githubusercontent.com"
  client_id_list = ["sts.amazonaws.com"]

  # Since mid-2023 AWS validates this issuer against its own trust store and
  # ignores the thumbprint, but the API still requires the field. Deriving it
  # from the live cert beats hardcoding a constant that silently rots.
  thumbprint_list = [data.tls_certificate.github_actions.certificates[0].sha1_fingerprint]
}
