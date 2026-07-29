#!/usr/bin/env sh
# Regenerates src/studio/assets/vendor/three.bundle.js.
#
# The studio is one HTML file that has to work from `file://`, so three.js
# cannot arrive as an ES module: the published `three.module.min.js` imports
# `./three.core.min.js`, and a relative module import has nothing to resolve
# against inside an inlined document. Chrome also refuses module scripts on
# `file://` outright. So three is bundled here into a classic IIFE that assigns
# one global, which loads from a `<script>` tag anywhere.
#
# The bundle is committed. This script exists so that bumping three.js is a
# reproducible step rather than an archaeology exercise -- it needs node only
# when run, never to build or use `brain`.
#
#   sh tools/vendor-three.sh            # current pinned version
#   THREE_VERSION=0.186.0 sh tools/...  # bump

set -eu

THREE_VERSION="${THREE_VERSION:-0.185.1}"
out="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)/src/studio/assets/vendor/three.bundle.js"

command -v npx >/dev/null 2>&1 || { echo "vendor-three: needs node/npx on PATH" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

npm init -y >/dev/null 2>&1
npm install --no-audit --no-fund --silent "three@${THREE_VERSION}" esbuild >/dev/null 2>&1

# Naming every import keeps the bundle tree-shaken to what the studio actually
# draws. Adding a three.js symbol to studio.js means adding it here too, or it
# is simply absent at runtime.
cat > entry.mjs <<'EOF'
export {
  Scene, PerspectiveCamera, WebGLRenderer, Color, Vector2, Vector3, Matrix4, Quaternion,
  Group, Object3D, BufferGeometry, BufferAttribute, Float32BufferAttribute,
  IcosahedronGeometry, SphereGeometry, PlaneGeometry, RingGeometry,
  InstancedMesh, Mesh, Points, LineSegments,
  MeshBasicMaterial, MeshStandardMaterial,
  LineBasicMaterial, PointsMaterial, ShaderMaterial, SpriteMaterial, Sprite,
  CanvasTexture, AdditiveBlending, NormalBlending, DoubleSide,
  AmbientLight, DirectionalLight, PointLight, HemisphereLight,
  Raycaster, Clock, MathUtils, FogExp2, InstancedBufferAttribute,
  DynamicDrawUsage, SRGBColorSpace, ACESFilmicToneMapping,
} from 'three';
export { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
EOF

npx esbuild entry.mjs \
  --bundle --format=iife --global-name=THREE \
  --minify --legal-comments=inline \
  --outfile=three.bundle.js

cp three.bundle.js "$out"
echo "vendor-three: three@${THREE_VERSION} -> $out ($(wc -c < "$out") bytes)"
