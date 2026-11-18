FROM archlinux:base-devel@sha256:624bdc7fc6615155d67bc34cf7146c0c9fd0e3c92943977da63a630e4ca49c30

ARG CI_UID=1001
ARG RUST_VERSION

RUN pacman -Syu --noconfirm --needed \
    clang cmake curl dbus ffmpeg fontconfig freetype2 git gnome-keyring \
    gst-libav gst-plugins-bad gst-plugins-base gst-plugins-good \
    libxkbcommon libxkbcommon-x11 mesa niri noto-fonts-emoji \
    pipewire pipewire-audio python rustup ttf-dejavu unzip \
    vulkan-swrast wayland weston wireplumber wpewebkit \
    && pacman -Scc --noconfirm \
    && useradd --create-home --uid "$CI_UID" ci
# the Docker client: the server's SDK bot tests run their images on the runner's Docker
RUN pacman -S --noconfirm --needed docker && pacman -Scc --noconfirm

USER ci
ENV HOME="/home/ci" PATH="/home/ci/.cargo/bin:${PATH}"
RUN test -n "$RUST_VERSION" \
    && rustup default "$RUST_VERSION" \
    && rustup component add llvm-tools-preview \
    && cargo install --locked cargo-llvm-cov --version 0.9.1

ENV TZ="Etc/UTC"
COPY --chmod=755 run.sh /usr/local/bin/coverage-ci
COPY --chmod=755 smoke.sh /usr/local/bin/coverage-ci-smoke
COPY --chmod=755 enter.sh /usr/local/bin/coverage-ci-enter
WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/coverage-ci-enter"]
CMD ["/usr/local/bin/coverage-ci"]
