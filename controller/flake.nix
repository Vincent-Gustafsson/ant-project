
{
    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
        flake-utils.url = "github:numtide/flake-utils";
    };
    outputs =
        {
            self,
            nixpkgs,
            flake-utils,
            ...
        }:
        flake-utils.lib.eachDefaultSystem (
            system:
            let
                pkgs = import nixpkgs { inherit system; };
            in
            {
                devShells.default = pkgs.mkShell {
                    shellHook = "export NIX_SHELL_NAME='controller'";
                    nativeBuildInputs = with pkgs; [
                        rustup
                        pkg-config
                    ];
                    buildInputs = with pkgs; [
                        wget
                        libarchive
                        openssl
                    ];
                };
            }
        );
}
