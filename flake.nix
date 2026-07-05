{
  description = "wunderdrive — S3-compatible document store with desktop client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # iced/winit dlopen's these at runtime; NixOS doesn't put them in the
      # default search path, so the GUI panics without LD_LIBRARY_PATH.
      runtimeLibs = [ pkgs.wayland pkgs.libxkbcommon pkgs.fontconfig ];
    in {
      devShells.${system}.default = pkgs.mkShell {
        buildInputs = runtimeLibs;
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
      };
    };
}
