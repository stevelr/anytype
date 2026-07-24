{
  description = "Anytype rust tools and client library";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
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
          toolchain = inputs.fenix.packages.${system}.fromToolchainFile {
            file = ./rust-toolchain.toml;
            sha256 = "sha256-A1abGIbOtcBSdrUMhDGrER3pRM1hQP4fp9gh3Y4PKc8=";
          };
          toolchainNightly = inputs.fenix.packages.${system}.latest.toolchain;
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

        in
        {
          packages.default = anytype-toolbox;
          devShells.default = pkgs.mkShell {
            nativeBuildInputs = [
              pkgs.jq
              pkgs.just
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
