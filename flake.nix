{
  description = "Development environment for Nomos.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/02c80fc5421018016669d79765b40a18aaf3bd8d";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/a9471b23bf656d69ceb2d5ddccdc5082d51fc0e3";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-darwin" "x86_64-windows" ];
      forAll = fn: builtins.listToAttrs (map (system: { name = system; value = fn system; }) systems);

    in
    {
      devShells = forAll (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

        in
        {
          default = self.devShells.${system}.research;
          research = pkgs.mkShell {
            name = "research";
            buildInputs = with pkgs; [
              pkg-config
              # Updating the version here requires also updating the `rev` version in the `overlays` section above
              # with a commit that contains the new version in its manifest
              rust-bin.stable."1.92.0".default
              clang_14
              llvmPackages_14.libclang
              openssl.dev
            ];
            shellHook = ''
              export LIBCLANG_PATH="${pkgs.llvmPackages_14.libclang.lib}/lib";
            '';
          };
        }
      );
    };
}
