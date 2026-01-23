{
  description = "Development environment for Logos blockchain node.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/02c80fc5421018016669d79765b40a18aaf3bd8d";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/470ee44393bb19887056b557ea2c03fc5230bd5a";
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
              rust-bin.stable."1.93.0".default
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
