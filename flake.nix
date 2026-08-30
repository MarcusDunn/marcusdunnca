{
  description = "marcusdunnca — AWS infrastructure managed with OpenTofu";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            opentofu
            awscli2
            gh
            jq

            # The application toolchain. Absent originally because the handlers
            # and the SPA are built in CI — which meant the only way to learn
            # that a handler did not compile, or read an environment variable
            # the infrastructure never set, was to deploy it. Two production
            # 502s came from that gap.
            #
            # This is deliberately not the Lambda build: that targets
            # aarch64-unknown-linux-musl and stays in the workflow, where the
            # runner architecture matches the function's. What this gives is
            # `cargo check`, `cargo test` and `cargo clippy` before pushing.
            cargo
            rustc
            rustfmt
            nodejs
            pnpm

            # No clippy. The nixpkgs-unstable derivation is currently built
            # against rustc 1.94.0-beta.1 while cargo and rustc here are 1.97.1,
            # and cargo-clippy refuses outright on the mismatch:
            #
            #   rustc 1.94.0-beta.1 is not supported by the following packages:
            #     api@0.1.0 requires rustc 1.94.1
            #
            # Shipping a tool that cannot run is worse than not shipping it —
            # it reads as "clippy is available here" and fails at the moment
            # someone tries to use it.

            # webauthn-rs depends on openssl-sys, which has no pure-Rust
            # backend and locates the library through pkg-config. Without both
            # of these the api crate does not build at all — which is why CI
            # installs `perl` and `make` and compiles OpenSSL from source under
            # the `vendored-openssl` feature. Locally the system library is
            # fine and very much faster.
            pkg-config
            openssl
          ];

          # The `marcusdunnca` profile is declared in nix-config
          # (modules/aws.nix), so ~/.aws/config stays the single source of truth.
          # Defaulting AWS_PROFILE here means commands in this shell target the
          # personal account without anyone remembering a flag, while the work
          # profiles on this machine stay one explicit override away.
          #
          # This is convenience, not a guarantee — the real guard against applying
          # to the wrong account is the account-ID assertion in scripts/bootstrap.sh.
          shellHook = ''
            export AWS_PROFILE="marcusdunnca"
            unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
            echo "marcusdunnca devshell — tofu $(tofu version | head -1 | cut -d' ' -f2), profile $AWS_PROFILE"
          '';
        };
      });
    };
}
