{
  description = "Powerline-styled path, git status, and tmux title segments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
    }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

      # Track CI's `dtolnay/rust-toolchain@stable`. Bumping rust-overlay
      # (via `nix flake update rust-overlay`) advances both the devShell
      # and the build in lockstep with what CI's stable resolves to.
      rustToolchainFor = system: (pkgsFor system).rust-bin.stable.latest.default;

      craneLibFor =
        system:
        (crane.mkLib (pkgsFor system)).overrideToolchain (rustToolchainFor system);

      buildFor =
        system:
        let
          pkgs = pkgsFor system;
          craneLib = craneLibFor system;
        in
        craneLib.buildPackage {
          src =
            let
              binFilter = path: _type: builtins.match ".*\\.bin$" path != null;
              # insta snapshot files end in .snap; the cargo source filter
              # rejects them by default and tests fail in the sandbox.
              snapFilter = path: _type: builtins.match ".*\\.snap$" path != null;
            in
            pkgs.lib.cleanSourceWith {
              src = ./.;
              filter =
                path: type:
                (binFilter path type)
                || (snapFilter path type)
                || (craneLib.filterCargoSources path type);
            };
          strictDeps = true;
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.cmake
          ];
          buildInputs =
            [
              pkgs.openssl
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.apple-sdk_15
              pkgs.libiconv
            ];
        };
    in
    {
      packages = forAllSystems (system: {
        default = buildFor system;
        plx = buildFor system;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          rust = rustToolchainFor system;
        in
        {
          # Pinned rust + the deps needed to build plx interactively.
          # Apple SDK is intentionally NOT pulled in here: rust-overlay's
          # rustc binds its own SDK via DEVELOPER_DIR and adding apple-sdk
          # collides with that. The crane build (above) still uses
          # apple-sdk_15 because it runs in its own sandbox.
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.pkg-config
              pkgs.cmake
              pkgs.openssl
              pkgs.lefthook
            ];
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt-tree);
    };
}
