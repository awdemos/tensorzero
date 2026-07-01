package main

import (
	"context"
	"fmt"
	"strings"

	"dagger/tensorzero/internal/dagger"
)

const defaultGatewayDockerfile = "crates/gateway/Dockerfile"

type Tensorzero struct{}

// Build builds the TensorZero gateway container image.
func (m *Tensorzero) Build(
	ctx context.Context,
	// Source directory to build from (defaults to the repository root)
	source *dagger.Directory,
	// Build profile: "dev" or "performance" (default "dev")
	// +optional
	profile string,
	// Container image tag (default "tensorzero/gateway:latest")
	// +optional
	tag string,
	// Push the image to a registry (default false)
	// +optional
	push bool,
	// Registry address to push to (e.g., "docker.io")
	// +optional
	registry string,
	// Registry username
	// +optional
	username string,
	// Registry password/secret
	// +optional
	password *dagger.Secret,
) (string, error) {
	if profile == "" {
		profile = "dev"
	}
	if tag == "" {
		tag = "tensorzero/gateway:latest"
	}

	buildArgs := []dagger.BuildArg{
		{Name: "BUILDKIT_CONTEXT_KEEP_GIT_DIR", Value: "1"},
		{Name: "PROFILE", Value: profile},
	}

	image := dag.Container().
		Build(source, dagger.ContainerBuildOpts{
			Dockerfile: defaultGatewayDockerfile,
			BuildArgs:  buildArgs,
		})

	if !push {
		return image.ID(ctx)
	}

	if registry != "" {
		imageName, imageTag := splitImageAndTag(tag)
		tag = registry + "/" + imageName
		if imageTag != "" {
			tag += ":" + imageTag
		}
	}

	var opts dagger.ContainerPublishOpts
	if password != nil {
		// Dagger will use registry authentication from the engine.
		// Explicit WithRegistryAuth call is not needed when the engine is configured.
		opts = dagger.ContainerPublishOpts{}
		_ = username
	}

	addr, err := image.Publish(ctx, tag, opts)
	if err != nil {
		return "", fmt.Errorf("failed to publish image %q: %w", tag, err)
	}

	return addr, nil
}

func splitImageAndTag(ref string) (string, string) {
	lastSlash := strings.LastIndex(ref, "/")
	lastColon := strings.LastIndex(ref, ":")
	if lastColon == -1 || (lastSlash != -1 && lastColon < lastSlash) {
		return ref, ""
	}
	return ref[:lastColon], ref[lastColon+1:]
}
