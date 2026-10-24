# The image the UI scenarios run in locally: matterfast's (the host's
# distribution with sway, grim and Mesa's software Vulkan) and git, which the
# demo's sample repository and the review need. CI runs them on the runner
# itself, which has git.
#
#   podman build -t localhost/aiddiction-ui -f build-aux/ui-tests.Containerfile build-aux
FROM localhost/matterfast-ui
RUN dnf install -y --setopt=install_weak_deps=False git && dnf clean all
