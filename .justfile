# Get the current user ID and group ID
uid := `id -u`
gid := `id -g`

default: test
  @just --list

# Test suite
test: clippy fmt
  @echo "🧪 Checking MariaDB..."
  @if ! podman ps --filter "name=mariadb_exporter_db" --format "{{{{.Names}}}}" | grep -q "mariadb_exporter_db"; then \
    echo "🚀 MariaDB container not running, starting it..."; \
    just mariadb; \
    echo "⏳ Waiting for MariaDB to be ready..."; \
    sleep 3; \
    timeout 30 bash -c 'until podman exec mariadb_exporter_db mariadb-admin ping -h "127.0.0.1" -proot --silent; do sleep 1; done' || (echo "❌ MariaDB failed to start" && exit 1); \
    echo "✅ MariaDB is ready"; \
  else \
    echo "✅ MariaDB container is already running"; \
  fi
  @echo "🧪 Running setup check..."
  @if [ -f scripts/setup-local-test-db.sh ]; then \
    scripts/setup-local-test-db.sh || (echo "❌ Test database setup failed. Fix the issues above before running tests." && exit 1); \
  fi
  @echo "🔧 Using local test database (overriding .envrc)..."
  MARIADB_EXPORTER_DSN="mysql://root:root@127.0.0.1:3306/mysql" cargo test -- --nocapture

# Linting
clippy:
  cargo clippy --all-targets --all-features

# Formatting check
fmt:
  cargo fmt --all -- --check

# Coverage report
coverage:
  CARGO_INCREMENTAL=0 RUSTFLAGS='-Cinstrument-coverage' LLVM_PROFILE_FILE='coverage-%p-%m.profraw' cargo test
  grcov . --binary-path ./target/debug/deps/ -s . -t html --branch --ignore-not-existing --ignore '../*' --ignore "/*" -o target/coverage/html
  firefox target/coverage/html/index.html
  rm -rf *.profraw

# Update dependencies
update:
  cargo update

# Clean build artifacts
clean:
  cargo clean

# Get current version
version:
    @cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'

# Check if working directory is clean
check-clean:
    #!/usr/bin/env bash
    if [[ -n $(git status --porcelain) ]]; then
        echo "❌ Working directory is not clean. Commit or stash your changes first."
        git status --short
        exit 1
    fi
    echo "✅ Working directory is clean"

# Check if on develop branch
check-develop:
    #!/usr/bin/env bash
    current_branch=$(git branch --show-current)
    if [[ "$current_branch" != "develop" ]]; then
        echo "❌ Not on develop branch (currently on: $current_branch)"
        echo "Switch to develop branch first: git checkout develop"
        exit 1
    fi
    echo "✅ On develop branch"

# Check if tag already exists for a given version
check-tag-not-exists version:
    #!/usr/bin/env bash
    set -euo pipefail
    version="{{version}}"

    git fetch --tags --quiet

    if git rev-parse -q --verify "refs/tags/${version}" >/dev/null 2>&1; then
        echo "❌ Tag ${version} already exists!"
        exit 1
    fi

    echo "✅ No tag exists for version ${version}"

_bump bump_kind: check-develop check-clean clean update test
    #!/usr/bin/env bash
    set -euo pipefail

    bump_kind="{{bump_kind}}"

    cleanup() {
        status=$?
        if [ $status -ne 0 ]; then
            echo "↩️  Restoring version files after failure..."
            git checkout -- Cargo.toml Cargo.lock >/dev/null 2>&1 || true
        fi
        exit $status
    }
    trap cleanup EXIT

    previous_version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
    echo "ℹ️  Current version: ${previous_version}"

    echo "🔧 Bumping ${bump_kind} version..."
    cargo set-version --bump "${bump_kind}"
    new_version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
    echo "📝 New version: ${new_version}"

    validate_bump() {
        local previous=$1 bump=$2 current=$3
        IFS=. read -r prev_major prev_minor prev_patch <<<"${previous}"
        IFS=. read -r new_major new_minor new_patch <<<"${current}"

        case "${bump}" in
            patch)
                (( new_major == prev_major && new_minor == prev_minor && new_patch == prev_patch + 1 )) || { echo "❌ Expected patch bump from ${previous}, got ${current}"; exit 1; }
                ;;
            minor)
                (( new_major == prev_major && new_minor == prev_minor + 1 && new_patch == 0 )) || { echo "❌ Expected minor bump from ${previous}, got ${current}"; exit 1; }
                ;;
            major)
                (( new_major == prev_major + 1 && new_minor == 0 && new_patch == 0 )) || { echo "❌ Expected major bump from ${previous}, got ${current}"; exit 1; }
                ;;
        esac
    }

    validate_bump "${previous_version}" "${bump_kind}" "${new_version}"

    echo "🔍 Verifying tag does not exist for ${new_version}..."
    git fetch --tags --quiet
    if git rev-parse -q --verify "refs/tags/${new_version}" >/dev/null 2>&1; then
        echo "❌ Tag ${new_version} already exists!"
        exit 1
    fi

    echo "🔄 Updating dependencies..."
    cargo update

    echo "🧹 Running clean build..."
    cargo clean

    echo "🧪 Running tests with new version (via just test)..."
    just test

    git add .
    git commit -m "bump version to ${new_version}"
    git push origin develop
    echo "✅ Version bumped and pushed to develop"

# Bump version and commit (patch level)
bump:
    @just _bump patch

# Bump minor version
bump-minor:
    @just _bump minor

# Bump major version
bump-major:
    @just _bump major

# Internal function to handle the merge and tag process
_deploy-merge-and-tag:
    #!/usr/bin/env bash
    set -euo pipefail

    new_version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
    echo "🚀 Starting deployment for version $new_version..."

    # Double-check tag doesn't exist (safety check)
    echo "🔍 Verifying tag doesn't exist..."
    git fetch --tags --quiet
    if git rev-parse -q --verify "refs/tags/${new_version}" >/dev/null 2>&1; then
        echo "❌ Tag ${new_version} already exists on remote!"
        echo "This should not happen. The tag may have been created in a previous run."
        exit 1
    fi

    # Ensure develop is up to date
    echo "🔄 Ensuring develop is up to date..."
    git pull origin develop

    # Switch to main and merge develop
    echo "🔄 Switching to main branch..."
    git checkout main
    git pull origin main

    echo "🔀 Merging develop into main..."
    if ! git merge develop --no-edit; then
        echo "❌ Merge failed! Please resolve conflicts manually."
        git checkout develop
        exit 1
    fi

    # Create signed tag
    echo "🏷️  Creating signed tag $new_version..."
    git tag -s "$new_version" -m "Release version $new_version"

    # Push main and tag atomically
    echo "⬆️  Pushing main branch and tag..."
    if ! git push origin main "$new_version"; then
        echo "❌ Push failed! Rolling back..."
        git tag -d "$new_version"
        git checkout develop
        exit 1
    fi

    # Switch back to develop
    echo "🔄 Switching back to develop..."
    git checkout develop

    echo "✅ Deployment complete!"
    echo "🎉 Version $new_version has been released"
    echo "📋 Summary:"
    echo "   - develop branch: bumped and pushed"
    echo "   - main branch: merged and pushed"
    echo "   - tag $new_version: created and pushed"
    echo "🔗 Monitor release: https://github.com/nbari/mariadb_exporter/actions"

# Deploy: merge to main, tag, and push everything
deploy: bump _deploy-merge-and-tag

# Deploy with minor version bump
deploy-minor: bump-minor _deploy-merge-and-tag

# Deploy with major version bump
deploy-major: bump-major _deploy-merge-and-tag

# Create & push a test tag like t-YYYYMMDD-HHMMSS (skips publish/release in CI)
# Usage:
#   just t-deploy
#   just t-deploy "optional tag message"
t-deploy message="CI test": check-develop check-clean test
    #!/usr/bin/env bash
    set -euo pipefail

    message="{{message}}"
    ts="$(date -u +%Y%m%d-%H%M%S)"
    tag="t-${ts}"

    echo "🏷️  Creating signed test tag: ${tag}"
    git fetch --tags --quiet

    if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
        echo "❌ Tag ${tag} already exists. Aborting." >&2
        exit 1
    fi

    git tag -s "${tag}" -m "${message}"
    git push origin "${tag}"

    echo "✅ Pushed ${tag}"
    echo "🧹 To remove it:"
    echo "   git push origin :refs/tags/${tag} && git tag -d ${tag}"

# Watch for changes and run
watch:
  cargo watch -x 'run -- --collector.default --collector.exporter --collector.tls --collector.query_response_time --collector.statements --collector.schema --collector.replication --collector.locks --collector.metadata --collector.userstat --collector.innodb -v'

# get metrics curl
curl:
  curl -s 0:9306/metrics

mariadb version="11.4":
  mkdir -p db/data db/conf
  podman run --rm -d --name mariadb_exporter_db \
    -e MARIADB_ROOT_PASSWORD=root \
    -e MARIADB_ROOT_HOST=% \
    -e MARIADB_DATABASE=mysql \
    -p 3306:3306 \
    -v ${PWD}/db/conf:/etc/mysql/conf.d:Z \
    -v ${PWD}/db/data:/var/lib/mysql:Z \
    --health-cmd="mariadb-admin ping -h 127.0.0.1 -proot --silent" \
    --health-interval=10s \
    --health-timeout=5s \
    --health-retries=5 \
    --userns keep-id:uid={{ uid }},gid={{ gid }} \
    --user {{ uid }}:{{ gid }} \
    mariadb:{{ version }}

jaeger:
  podman run --rm -d --name jaeger \
    -e COLLECTOR_OTLP_ENABLED=true \
    -p 16686:16686 \
    -p 4317:4317 \
    -p 4318:4318 \
    jaegertracing/all-in-one:latest

stop-containers:
  @for c in metrics-stack mariadb_exporter_db jaeger; do \
        podman stop $c 2>/dev/null || true; \
  done

# Test against supported MariaDB LTS versions
test-all-mariadb:
    #!/usr/bin/env bash
    set -euo pipefail

    VERSIONS=(10.11 11.4 11.8)
    FAILED=()

    echo "🚀 Starting all MariaDB versions..."
    idx=0
    for v in "${VERSIONS[@]}"; do
        PORT=$((3310 + idx))
        NAME="mariadb${v//./}"
        podman run -d --name "${NAME}" \
            -e MARIADB_ROOT_PASSWORD=root \
            -e MARIADB_ROOT_HOST=% \
            -e MARIADB_DATABASE=mysql \
            -v ${PWD}/db/conf:/etc/mysql/conf.d:Z \
            -p ${PORT}:3306 \
            mariadb:${v} >/dev/null 2>&1 || true
        idx=$((idx + 1))
    done

    echo "⏳ Waiting for MariaDB instances to be ready..."
    sleep 5

    for v in "${VERSIONS[@]}"; do
        NAME="mariadb${v//./}"
        timeout 30 bash -c "until podman exec ${NAME} mariadb-admin ping -h 127.0.0.1 -proot --silent >/dev/null 2>&1; do sleep 1; done" || true
    done

    echo ""
    idx=0
    for v in "${VERSIONS[@]}"; do
        PORT=$((3306 + idx))
        NAME="mariadb${v//./}"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "🐬 Testing MariaDB ${v} (port ${PORT})"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

        if MARIADB_EXPORTER_DSN="mysql://root:root@127.0.0.1:${PORT}/mysql" \
           cargo test --quiet 2>&1 | tail -5; then
            echo "✅ MariaDB ${v} passed"
        else
            echo "❌ MariaDB ${v} failed"
            FAILED+=("${v}")
        fi
        idx=$((idx + 1))
        echo ""
    done

    echo "🧹 Cleaning up containers..."
    for v in "${VERSIONS[@]}"; do
        NAME="mariadb${v//./}"
        podman stop "${NAME}" >/dev/null 2>&1 || true
        podman rm "${NAME}" >/dev/null 2>&1 || true
    done

    if [ ${#FAILED[@]} -eq 0 ]; then
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "✅ All MariaDB versions passed!"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    else
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo "❌ Failed versions: ${FAILED[*]}"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        exit 1
    fi

# Test against specific MariaDB version
test-mariadb version:
    #!/usr/bin/env bash
    VERSION="{{version}}"
    PORT_SUFFIX="${VERSION//./}"
    PORT="33${PORT_SUFFIX: -2}"
    echo "🐬 Starting MariaDB ${VERSION} on port ${PORT}..."
    podman run -d --name mariadb${PORT_SUFFIX} \
        -e MARIADB_ROOT_PASSWORD=root \
        -e MARIADB_ROOT_HOST=% \
        -e MARIADB_DATABASE=mysql \
        -p ${PORT}:3306 \
        -v ${PWD}/db:/etc/mysql/conf.d:Z \
        -v ${PWD}/db/data:/var/lib/mysql:Z \
        mariadb:${VERSION}

    echo "⏳ Waiting for MariaDB to be ready..."
    sleep 3
    timeout 30 bash -c "until podman exec mariadb${PORT_SUFFIX} mariadb-admin ping -h 127.0.0.1 -proot --silent >/dev/null 2>&1; do sleep 1; done"

    echo "🧪 Running tests..."
    MARIADB_EXPORTER_DSN="mysql://root:root@127.0.0.1:${PORT}/mysql" cargo test

    echo "🧹 Cleaning up..."
    podman stop mariadb${PORT_SUFFIX} && podman rm mariadb${PORT_SUFFIX}

# Validate Grafana dashboard
validate-dashboard:
  @./scripts/validate-dashboard.sh

# Run local Prometheus + Grafana stack against exporter (podman)
metrics target="host.containers.internal:9306" image="mariadb-exporter-stack" name="metrics-stack" prom_volume="prom_data":
  # Build the stack image if it's missing so `just metrics` works without a manual build step
  if ! podman image exists "{{image}}"; then \
    podman build -t "{{image}}" grafana; \
  fi
  # Replace any existing container with the same name to avoid conflicts
  podman rm -f {{name}} 2>/dev/null || true
  # Ensure a persistent volume for Prometheus data
  if ! podman volume exists "{{prom_volume}}"; then \
    podman volume create "{{prom_volume}}"; \
  fi
  podman run -d \
    --name {{name}} \
    --add-host=host.containers.internal:host-gateway \
    -e EXPORTER_TARGET={{target}} \
    -e GF_AUTH_ANONYMOUS_ENABLED=true \
    -e GF_AUTH_ANONYMOUS_ORG_ROLE=Admin \
    -e GF_SECURITY_DISABLE_LOGIN_FORM=true \
    -p 3000:3000 -p 9090:9090 \
    -v {{prom_volume}}:/var/lib/prometheus \
    {{image}}

restart-metrics target="host.containers.internal:9306" image="mariadb-exporter-stack" name="metrics-stack" prom_volume="prom_data":
  #!/usr/bin/env bash
  set -euo pipefail

  echo "🛑 Stopping existing metrics-stack container..."
  podman rm -f {{name}} 2>/dev/null || true

  echo "🔍 Checking if dashboard.json changed..."
  REBUILD=0

  # Check if image exists
  if ! podman image exists "{{image}}"; then
    echo "📦 Image doesn't exist, will build..."
    REBUILD=1
  else
    # Get image creation time
    IMAGE_TIME=$(podman inspect {{image}} --format='{{{{.Created}}}}' 2>/dev/null || echo "")

    if [ -n "$IMAGE_TIME" ]; then
      # Convert image time to epoch (works with ISO 8601 format)
      IMAGE_EPOCH=$(date -d "$IMAGE_TIME" +%s 2>/dev/null || echo "0")

      # Get dashboard.json modification time
      DASHBOARD_EPOCH=$(stat -c %Y grafana/dashboard.json 2>/dev/null || echo "0")

      if [ "$DASHBOARD_EPOCH" -gt "$IMAGE_EPOCH" ]; then
        echo "📊 Dashboard changed ($(date -d @$DASHBOARD_EPOCH '+%Y-%m-%d %H:%M:%S') > $(date -d @$IMAGE_EPOCH '+%Y-%m-%d %H:%M:%S')), rebuilding image..."
        REBUILD=1
      else
        echo "✅ Dashboard unchanged, reusing existing image"
      fi
    else
      echo "⚠️  Could not get image time, rebuilding to be safe..."
      REBUILD=1
    fi
  fi

  if [ "$REBUILD" -eq 1 ]; then
    echo "🔨 Building Grafana stack image..."
    podman build -t "{{image}}" grafana
  fi

  if ! podman volume exists "{{prom_volume}}"; then
    echo "📦 Creating Prometheus volume..."
    podman volume create "{{prom_volume}}"
  fi

  echo "🚀 Starting metrics-stack container..."
  podman run -d \
    --name {{name}} \
    --add-host=host.containers.internal:host-gateway \
    -e EXPORTER_TARGET={{target}} \
    -e GF_AUTH_ANONYMOUS_ENABLED=true \
    -e GF_AUTH_ANONYMOUS_ORG_ROLE=Admin \
    -e GF_SECURITY_DISABLE_LOGIN_FORM=true \
    -p 3000:3000 -p 9090:9090 \
    -v {{prom_volume}}:/var/lib/prometheus \
    {{image}}

  echo "✅ Metrics stack restarted!"
  echo "🌐 Grafana: http://localhost:3000"
  echo "📊 Prometheus: http://localhost:9090"

# Run all validations (tests + dashboard)
validate-all: test validate-dashboard
  @echo "✅ All validations passed!"

# Build container image (standalone exporter)
build-image:
  podman build -t mariadb_exporter:latest -f Containerfile .

# Test multi-arch container build (like GitHub Actions, without pushing)
test-container-build:
  #!/usr/bin/env bash
  set -euo pipefail

  VERSION=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
  echo "🐳 Testing container build for version ${VERSION}"

  # Build for native platform only (for local testing without QEMU)
  # CI/CD will build multi-arch with --platform linux/amd64,linux/arm64
  podman build \
    -t mariadb_exporter:test-${VERSION} \
    -f Containerfile \
    .

  echo "✅ Container build successful!"
  echo "Test with: podman run --rm mariadb_exporter:test-${VERSION} --version"
  echo "📦 Manifest: mariadb_exporter:test-${VERSION}"
  echo ""
  echo "🔍 To inspect:"
  echo "   podman manifest inspect mariadb_exporter:test-${VERSION}"
  echo ""
  echo "🧹 To clean up:"
  echo "   podman manifest rm mariadb_exporter:test-${VERSION}"

# Build combined MariaDB + exporter image (realistic socket testing)
build-image-combined:
  podman build -t mariadb_exporter:combined -f Containerfile.mariadb .

# Test socket connection (combined container - most realistic)
test-socket:
  #!/usr/bin/env bash
  set -euo pipefail

  echo "🔧 Building combined MariaDB + exporter image..."
  just build-image-combined

  echo ""
  echo "🚀 Starting combined container..."
  podman run --rm -d --name mariadb_socket_test \
    -e MARIADB_ROOT_PASSWORD=root \
    -e MARIADB_DATABASE=mysql \
    -p 3307:3306 \
    -p 9308:9306 \
    mariadb_exporter:combined

  echo "⏳ Waiting for services to be ready..."
  sleep 10

  # Wait for MariaDB
  timeout 30 bash -c '
    until podman exec mariadb_socket_test mariadb-admin ping -h localhost -uroot -proot --silent 2>/dev/null; do
      sleep 1
    done
  ' || {
    echo "❌ MariaDB failed to start"
    podman logs mariadb_socket_test
    podman stop mariadb_socket_test || true
    exit 1
  }

  # Wait for exporter
  timeout 30 bash -c '
    until podman exec mariadb_socket_test wget -qO- http://localhost:9306/health 2>/dev/null | grep -q "ok"; do
      sleep 1
    done
  ' || {
    echo "❌ Exporter failed to start"
    podman logs mariadb_socket_test | tail -50
    podman stop mariadb_socket_test || true
    exit 1
  }

  echo "✅ Both services are ready!"
  echo ""

  # Test metrics
  echo "🧪 Testing metrics via socket connection..."
  if podman exec mariadb_socket_test wget -qO- http://localhost:9306/metrics | grep -q "mariadb_up 1"; then
    echo "✅ Socket connection successful!"
    echo ""
    echo "📊 Sample metrics:"
    podman exec mariadb_socket_test wget -qO- http://localhost:9306/metrics | grep -E "^mariadb_(up|version|exporter)" | head -8
  else
    echo "❌ Socket connection failed!"
    podman logs mariadb_socket_test | tail -50
    podman stop mariadb_socket_test || true
    exit 1
  fi

  echo ""
  echo "🧹 Cleaning up..."
  podman stop mariadb_socket_test || true
  echo "✅ Test complete!"
