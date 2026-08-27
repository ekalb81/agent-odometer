# Pinned Linux CI image for Tauri/WebKit build dependencies (issue #78).
#
# The Linux lanes each install the same dependency set with apt, and that
# step is the single largest cost in the pipeline: 30.9 minutes total across
# a 25-run window, p50 31s and p95 59s per job, more than the coverage
# computation it exists to support.
#
# This image is DEFINED but NOT ADOPTED. #78 is explicit that adoption
# requires measurement first — "Adopt the image only if it improves the
# measured pipeline outcome" — and the pull time is the whole question. An
# image large enough to hold WebKit's dev headers may cost more to pull than
# the apt install it replaces.
#
# The `ci-image-probe` job in `.github/workflows/ci.yml` builds this,
# reports its size and build time, and times a dependency-consuming step
# inside it against the apt baseline. Run it with `workflow_dispatch`.
#
# Pinned by digest rather than tag: `ubuntu:22.04` moves, and an image whose
# base silently changes cannot serve as a stable comparison — which is the
# reproducibility requirement in #78's scope.
FROM ubuntu@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc

# Matches `.github/actions/linux-deps/action.yml` exactly. Kept in step with
# it deliberately: two lists that can drift are how a container-based lane
# starts passing while the apt-based lane fails, or the reverse.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev \
        libsoup-3.0-dev \
        librsvg2-dev \
        libayatana-appindicator3-dev \
        libgtk-3-dev \
        libssl-dev \
        build-essential \
        pkg-config \
        ca-certificates \
        curl \
        git \
    && rm -rf /var/lib/apt/lists/*

# A marker the probe asserts against, so a run cannot report a "warmed image"
# result while actually having fallen back to a plain Ubuntu container.
RUN echo "agent-odometer-ci-linux" > /etc/agent-odometer-ci-image

LABEL org.opencontainers.image.source="https://github.com/ekalb81/agent-odometer"
LABEL org.opencontainers.image.description="Tauri/WebKit build dependencies for agent-odometer CI (issue #78)"
