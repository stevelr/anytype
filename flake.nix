{
  description = "Anytype rust tools and client library";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    naersk.url = "github:nix-community/naersk";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      #rust-overlay,
      naersk,
    }:
    flake-utils.lib.eachSystem
      [
        "x86_64-linux"
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-windows"
      ]
      (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
          };
          naersk' = pkgs.callPackage naersk { };
        in
        {
          # build all bin targets
          packages.default = naersk'.buildPackage {
            src = ./.;
            PROTOC = "${pkgs.protobuf}/bin/protoc";
          };

          devShells.default = pkgs.mkShell {
            buildInputs = with pkgs; [
              pkg-config
              protobuf
              rustc
              cargo
              chafa
              # build static library that anyback can link
              pkgsStatic.chafa
            ];
            PROTOC = "${pkgs.protobuf}/bin/protoc";
            PROTOC_INCLUDE = "${pkgs.protobuf}/include";
            NIX_ENFORCE_PURITY = 0;
          };
        }
      );
}
