{
  description = "Anytype rust tools and client library";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      #inputs.nixpkgs.follows = "nixpkgs";
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
          mkAnytypeBin =
            {
              name,
              version,
              packageSet ? pkgs,
              rustNightlyToolchain ? toolchainNightly,
              rustToolchain ? toolchain,
              buildFeatures ? [ ],
              doCheck ? true,
              cargoBuildFlags ? [ ],
              nightly ? false,
            }:
            let
              buildProtobuf = packageSet.buildPackages.protobuf;
            in
            (packageSet.makeRustPlatform {
              cargo = if nightly then rustNightlyToolchain else rustToolchain;
              rustc = if nightly then rustNightlyToolchain else rustToolchain;
            }).buildRustPackage
              {
                inherit
                  name
                  version
                  buildFeatures
                  cargoBuildFlags
                  doCheck
                  ;
                #postInstall = lib.optionalString installManPages manPagesPostInstall + postInstall;
                cargoLock.lockFile = ./Cargo.lock;
                src = ./.;
                nativeBuildInputs = [ buildProtobuf ];
                PROTOC = "${buildProtobuf}/bin/protoc";
                PROTOC_INCLUDE = "${buildProtobuf}/include";
                SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
                meta = with packageSet.lib; {
                  description = "anytype FIXME";
                  homepage = "https://github.com/stevelr/anytype";
                  license = licenses.asl20;
                  mainProgram = "anyr";
                };
              };
          anyr-version = (fromTOML (builtins.readFile ./anyr/Cargo.toml)).package.version;
          any-mcp-version = (fromTOML (builtins.readFile ./anyr/Cargo.toml)).package.version;
          #lib-version = (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          any-mcp = mkAnytypeBin {
            name = "any-mcp";
            version = any-mcp-version;
            cargoBuildFlags = [
              "--package"
              "any-mcp"
            ];
          };
          anyr = mkAnytypeBin {
            name = "anyr";
            version = anyr-version;
            cargoBuildFlags = [
              "--package"
              "anyr"
            ];
          };
          any-edit = mkAnytypeBin {
            name = "any-edit";
            version = (fromTOML (builtins.readFile ./any-edit/Cargo.toml)).package.version;
            cargoBuildFlags = [
              "--package"
              "any-edit"
            ];
          };
          anyback = mkAnytypeBin {
            name = "anyback";
            version = (fromTOML (builtins.readFile ./anyback/Cargo.toml)).package.version;
            cargoBuildFlags = [
              "--package"
              "anyback"
            ];
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
        in
        {
          packages.any-edit = any-edit;
          packages.anyr = anyr;
          packages.anyback = anyback;
          packages.any-mcp = pkgs.symlinkJoin {
            name = "any-mcp";
            version = any-mcp-version;
            paths = [
              any-mcp
              anyr
              anytype-cli
            ];
          };
          #packages.anytype-cli = anytype-cli;
          packages.default = pkgs.symlinkJoin {
            name = "anytype";
            version = anyr-version;
            paths = [
              any-mcp
              any-edit
              anyr
              anyback
            ];
          };
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
