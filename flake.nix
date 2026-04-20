{
  description = "IoT-AtHome — reproducible dev shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Rust toolchain honors rust-toolchain.toml at repo root.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Python for the ML service (pinned minor version).
        python = pkgs.python312;
      in
      {
        devShells.default = pkgs.mkShell {
          name = "iot-athome-dev";

          packages = with pkgs; [
            # --- Rust ---
            rustToolchain
            cargo-nextest
            cargo-audit
            cargo-cyclonedx
            cargo-deny
            cargo-edit
            sqlx-cli

            # --- Protobuf / schemas ---
            protobuf
            buf

            # --- WASM / plugins ---
            wasmtime
            wasm-tools
            wabt

            # --- Node / Panel ---
            nodejs_22
            nodePackages.pnpm
            nodePackages.typescript

            # --- Python / ML service ---
            python
            uv
            poetry

            # --- Infra (dev stack) ---
            natscli
            mosquitto
            docker-compose

            # --- Crypto / signing ---
            cosign
            age
            openssl
            step-cli

            # --- Docs / observability tools ---
            jq
            yq-go
            minijinja-cli

            # --- Task runner ---
            just

            # --- General ---
            git
            gnumake
            pkg-config
            # ESP-IDF omitted from the default shell to keep it lean.
            # Use `nix develop .#firmware` when working on ESP32 code.
          ];

          shellHook = ''
            export IOT_DEV_ROOT="$PWD"
            export PATH="$IOT_DEV_ROOT/tools/bin:$PATH"
            echo
            echo "IoT-AtHome dev shell — rust $(rustc --version | cut -d' ' -f2), node $(node --version), python $(python3 --version | cut -d' ' -f2)"
            echo "Run 'just' to see available tasks."
            echo
          '';
        };

        # Firmware shell with ESP-IDF (heavier; opt-in).
        devShells.firmware = pkgs.mkShell {
          name = "iot-athome-firmware";
          packages = with pkgs; [
            rustToolchain
            esp-idf-esp32
            esp-idf-esp32s3
            platformio
            cosign
            just
          ];
        };
      });
}
