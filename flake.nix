{
  description = "opal — screenshot tool for Wayland with a liquid-glass annotation UI";

  inputs.nixpkgs.url = "nixpkgs";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        opal = pkgs.callPackage ./nix/package.nix { };
        default = opal;
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer pkg-config ];
          buildInputs = with pkgs; [ wayland libxkbcommon fontconfig freetype ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ wayland libxkbcommon fontconfig ]);
        };
      });

      overlays.default = final: _prev: {
        opal = final.callPackage ./nix/package.nix { };
      };
    };
}
