# syntax=docker/dockerfile:1.4
# Runtime stage - binaries are pre-compiled via cross and passed via build context
FROM registry.suse.com/bci/bci-minimal:15.7

ARG TARGETARCH
ARG BINARY=susecon-beer-agent

# Copy the pre-compiled binaries for the target architecture
COPY --from=binaries linux/${TARGETARCH}/susecon-beer-agent /usr/local/bin/susecon-beer-agent

USER 1001

ENTRYPOINT ["/usr/local/bin/susecon-beer-agent"]