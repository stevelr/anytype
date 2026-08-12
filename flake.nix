{
  description = "Anytype rust tools and client library";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    gate-check = {
      url = "github:stevelr/gate-check";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:
    inputs.flake-utils.lib.eachSystem
      [
        "x86_64-linux"
        "aarch64-darwin"
        "aarch64-linux"
      ]
      (
        system:
        let
          pkgs = inputs.nixpkgs.legacyPackages.${system};
          lib = pkgs.lib;
          os = if pkgs.stdenv.isDarwin then "darwin" else "linux";
          arch = if pkgs.stdenv.isx86_64 then "amd64" else "arm64";
          dockerArch =
            {
              "x86_64-linux" = "amd64";
              "aarch64-linux" = "arm64";
            }
            .${system};
          toolchain = inputs.fenix.packages.${system}.fromToolchainFile {
            file = ./rust-toolchain.toml;
            sha256 = "sha256-A1abGIbOtcBSdrUMhDGrER3pRM1hQP4fp9gh3Y4PKc8=";
          };
          toolchainNightly = inputs.fenix.packages.${system}.latest.toolchain;
          # Static-musl target for portable Linux binaries: the glibc build's
          # ELF interpreter points into /nix/store, so it only runs on Nix
          # systems or inside the OCI images. The musl build is fully static
          # and is the release provenance for install scripts and packages.
          muslTarget =
            {
              "x86_64-linux" = "x86_64-unknown-linux-musl";
              "aarch64-linux" = "aarch64-unknown-linux-musl";
            }
            .${system} or null;
          # The musl std ships in the toolchain itself: rust-toolchain.toml
          # lists the musl targets, so fromToolchainFile includes them without
          # the import-from-derivation that per-target channel lookups need
          # (cross-system `nix flake check` must stay evaluation-only).
          workspace-version = (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          mkAnytypeToolbox =
            {
              version ? workspace-version,
              doCheck ? true,
              cargoBuildFlags ? [ ],
              nightly ? false,
            }:
            (pkgs.makeRustPlatform {
              cargo = if nightly then toolchainNightly else toolchain;
              rustc = if nightly then toolchainNightly else toolchain;
            }).buildRustPackage
              {
                inherit
                  version
                  cargoBuildFlags
                  doCheck
                  ;
                name = "Anytype Toolbox";
                #postInstall = lib.optionalString installManPages manPagesPostInstall + postInstall;
                # Portable macOS binaries must reference only dyld-shared-cache
                # install names. Nix links its own libiconv dylib; the system
                # libiconv exports the same interface, so the reference is
                # rewritten and the binary re-signed (install_name_tool
                # invalidates the ad-hoc signature).
                postFixup = pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
                  for binary in "$out"/bin/*; do
                    for dylib in $(otool -L "$binary" | grep -o '/nix/store/[^ ]*libiconv[^ ]*\.dylib' || true); do
                      install_name_tool -change "$dylib" /usr/lib/libiconv.2.dylib "$binary"
                    done
                    if type -t signDarwinBinariesIn > /dev/null; then
                      signDarwinBinariesIn "$(dirname "$binary")"
                    fi
                  done
                '';
                cargoLock.lockFile = ./Cargo.lock;
                src = ./.;
                nativeBuildInputs = [ pkgs.protobuf ];
                PROTOC = "${pkgs.protobuf}/bin/protoc";
                PROTOC_INCLUDE = "${pkgs.protobuf}/include";
                SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
                meta = with pkgs.lib; {
                  description = "anytype Rust Tools";
                  homepage = "https://github.com/stevelr/anytype";
                  license = licenses.asl20;
                  mainProgram = "anyr";
                };
              };
          anytype-cli = pkgs.stdenv.mkDerivation (
            let
              version = "0.3.6";
              sha256 =
                {
                  "linux-amd64" = "sha256-hisyrZ+8geqaEe525jC0b4sTNSeNTGC1FCc1UpBvM80=";
                  "linux-arm64" = "sha256-/mKD5O6DKEh29jKe/nNxFmvy+pSL9BVyV0FvOyPhEbs=";
                  "darwin-amd64" = "sha256-+RD1VocE45Q3ovb8wu3gCnVnsatpR/wJdoXO6LqHzT4=";
                  "darwin-arm64" = "sha256-oPBtq/WUytKuEUJEHFzKtRnPDcQ53gms3ga/hvR7mvo=";
                }
                ."${os}-${arch}";
            in
            {
              name = "anytype-cli";
              sourceRoot = ".";
              src = builtins.fetchurl {
                url = "https://github.com/anyproto/anytype-cli/releases/download/v${version}/anytype-cli-v${version}-${os}-${arch}.tar.gz";
                inherit sha256;
              };
              installPhase = ''
                runHook preInstall
                install -D -m755 anytype $out/bin/anytype
                runHook postInstall
              '';
              meta = with lib; {
                description = "Command-line client for Anytype";
                homepage = "https://github.com/anyproto/anytype-cli";
                license = licenses.mit;
                sourceProvenance = with sourceTypes; [ binaryNativeCode ];
                mainProgram = "anytype";
              };
            }
          );
          anytype-toolbox = mkAnytypeToolbox {
            version = workspace-version;
            nightly = false;
          };
          anytype-toolbox-binaries = mkAnytypeToolbox {
            version = workspace-version;
            cargoBuildFlags = [
              "--package"
              "anyr"
              "--bin"
              "anyr"
            ];
            doCheck = false;
            nightly = false;
          };
          anytype-toolbox-static =
            if muslTarget == null then
              null
            else
              (pkgs.pkgsStatic.makeRustPlatform {
                cargo = toolchain;
                rustc = toolchain;
              }).buildRustPackage
                {
                  name = "Anytype Toolbox Static";
                  version = workspace-version;
                  cargoLock.lockFile = ./Cargo.lock;
                  src = ./.;
                  cargoBuildFlags = [
                    "--package"
                    "anyr"
                    "--bin"
                    "anyr"
                  ];
                  doCheck = false;
                  nativeBuildInputs = [ pkgs.protobuf ];
                  PROTOC = "${pkgs.protobuf}/bin/protoc";
                  PROTOC_INCLUDE = "${pkgs.protobuf}/include";
                  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
                  # Vendored C (libdbus) compiled with GCC outline atomics
                  # references __aarch64_ldadd*_sync helpers that the static
                  # musl link does not provide; inline atomics avoid them.
                  env = lib.optionalAttrs (muslTarget == "aarch64-unknown-linux-musl") {
                    NIX_CFLAGS_COMPILE = "-mno-outline-atomics";
                  };
                  meta = with pkgs.lib; {
                    description = "Portable static anyr binary (musl)";
                    homepage = "https://github.com/stevelr/anytype";
                    license = licenses.asl20;
                    mainProgram = "anyr";
                  };
                };

        in
        {
          packages = {
            default = anytype-toolbox;
            "anytype-toolbox-${system}" = anytype-toolbox-binaries;
          }
          // lib.optionalAttrs (muslTarget != null) {
            anytype-toolbox-static = anytype-toolbox-static;
          }
          // lib.optionalAttrs pkgs.stdenv.isLinux {
            anytype-toolbox-oci = pkgs.dockerTools.buildLayeredImage {
              name = "anytype-toolbox";
              tag = "${workspace-version}-${dockerArch}";
              contents = pkgs.buildEnv {
                name = "anytype-toolbox-root";
                paths = [ anytype-toolbox-binaries ];
                pathsToLink = [ "/bin" ];
              };
              config = {
                Entrypoint = [ "/bin/anyr" ];
                WorkingDir = "/";
                Labels = {
                  "org.opencontainers.image.description" = "Anytype automation CLI with MCP server commands";
                  "org.opencontainers.image.source" = "https://github.com/stevelr/anytype";
                  "org.opencontainers.image.title" = "anytype-toolbox";
                  "org.opencontainers.image.version" = workspace-version;
                };
              };
            };
          };
          devShells.default = pkgs.mkShell {
            nativeBuildInputs = [
              inputs.gate-check.packages.${system}.default
              pkgs.stdenv.cc
              pkgs.jq
              pkgs.just
              pkgs.python314
              pkgs.protobuf
              toolchain
              anytype-cli
            ];
            buildInputs = with pkgs; [
              pkg-config
              #chafa
              # build static library that anyback can link
              #pkgsStatic.chafa
            ];
            PROTOC = "${pkgs.protobuf}/bin/protoc";
            PROTOC_INCLUDE = "${pkgs.protobuf}/include";
            NIX_ENFORCE_PURITY = 0;
          };
        }
      );
}
