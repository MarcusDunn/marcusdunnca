# Registered passkeys. PUBLIC DATA — a credential ID and public key are what the
# browser hands any relying party during a ceremony, and neither can forge an
# assertion. Committed deliberately so CI can apply them; the signing key is the
# secret here, and it lives in SSM.
#
#   laptop  3_n2sKu6...  synced, discoverable   (backup_eligible: true)
#   phone   AQR9S5Dj...  device-bound           (backup_eligible: false)
#
# The laptop credential is the recoverable one. If the phone is lost its
# credential is gone for good, which is why two were enrolled before the
# registration window closed.
webauthn_credentials = <<-JSON
[{"cred":{"cred_id":"3_n2sKu6yizY440mVTQ3Zw","cred":{"type_":"ES256","key":{"EC_EC2":{"curve":"SECP256R1","x":"aQtimriv0Re14d4vq_2hkS6hIVCzTeNzGorRw2DzWc0","y":"g-cKQpqKr0XHUtN7PXJsvbXGZEbBJy0AKnS1FcnQIeo"}}},"counter":0,"transports":null,"user_verified":true,"backup_eligible":true,"backup_state":true,"registration_policy":"required","extensions":{"cred_protect":"Ignored","hmac_create_secret":"NotRequested","appid":"NotRequested","cred_props":{"Unsigned":{"rk":true}}},"attestation":{"data":"None","metadata":"None"},"attestation_format":"none"}},{"cred":{"cred_id":"AQR9S5DjyLdp3cAoJY3l6oB7Dp7z9Mde9NRffAbMW3h6JFfcN2Qa-aFeBrloHATXWd1e9qH9u63VQ0N7tOwbbNM","cred":{"type_":"ES256","key":{"EC_EC2":{"curve":"SECP256R1","x":"2hCeug808Oxf9cpBlix1ezXRjTzvYilNiJJIi6OqLlw","y":"jDpzDrpHyWH9RbEPYJq61smlIyT_rSUP0wZYvJ0LxNg"}}},"counter":0,"transports":null,"user_verified":true,"backup_eligible":false,"backup_state":false,"registration_policy":"required","extensions":{"cred_protect":"Ignored","hmac_create_secret":"NotRequested","appid":"NotRequested","cred_props":{"Unsigned":{"rk":false}}},"attestation":{"data":"None","metadata":"None"},"attestation_format":"none"}}]
JSON
