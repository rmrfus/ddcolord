{
  description = "ddcolord — keep a colord display profile in step with the monitor's OSD color mode";

  # Reuse the registry nixpkgs; consumers can override with inputs.nixpkgs.follows.
  inputs.nixpkgs.url = "flake:nixpkgs";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems
        (system: f nixpkgs.legacyPackages.${system});
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

      build = pkgs: extra: pkgs.rustPlatform.buildRustPackage ({
        pname = "ddcolord";
        inherit version;
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        # libdbus-sys finds libdbus through pkg-config.
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.dbus ];
      } // extra);
    in {
      packages = forAll (pkgs: rec {
        default = ddcolord;

        ddcolord = build pkgs {
          postInstall = ''
            install -Dm644 man/man1/ddcolord.1 $out/share/man/man1/ddcolord.1
            install -Dm644 man/man5/ddcolord.toml.5 $out/share/man/man5/ddcolord.toml.5
            install -Dm644 examples/benq-pd3220u.toml \
              $out/share/doc/ddcolord/examples/benq-pd3220u.toml

            # NixOS discovers units in lib; stdenv moves them to share and leaves a symlink.
            install -Dm644 systemd/ddcolord.service \
              $out/lib/systemd/user/ddcolord.service
            substituteInPlace $out/lib/systemd/user/ddcolord.service \
              --replace-fail '%h/.local/bin/ddcolord' "$out/bin/ddcolord"
          '';

          meta = with pkgs.lib; {
            description = "Keep a colord display profile in step with the monitor's OSD color mode, read over DDC/CI";
            homepage = "https://github.com/rmrfus/ddcolord";
            changelog = "https://github.com/rmrfus/ddcolord/releases";
            license = licenses.mit;
            mainProgram = "ddcolord";
            platforms = platforms.linux;
          };
        };
      });

      checks = forAll (pkgs: {
        # nix flake check builds checks.* but only evaluates packages.*, so the
        # cross build has to live here to be verified at all.
        cross-aarch64 = build pkgs.pkgsCross.aarch64-multiplatform {
          pname = "ddcolord-cross-aarch64";
          # Cross-built, so the test binaries cannot run on the builder.
          doCheck = false;
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            pkg-config
            groff         # man page lint: groff -man -Tutf8 -ww -z man/man*/*
            cargo-deny    # cargo deny check advisories sources
            cargo-machete # dependencies declared and never used
          ];
          buildInputs = [ pkgs.dbus ];
        };
      });
    };
}
