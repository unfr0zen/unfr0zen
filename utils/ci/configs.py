#!/usr/bin/env python3
"""The repository's CircleCI configs, one per subsystem with its own workflow,
and which of them a pipeline merges.

The setup workflow's path filtering sets a parameter per subsystem whose files
a push changed; each subsystem's config declares the parameter and gates its
workflow on it. A workflow lists the module jobs it needs as its own, so the
configs defining them are merged too (their workflows stay off): that is the
dependency table below. A release tag runs the release workflow alone, with
the image's. The build image's tag is the Dockerfile's SHA-256, twelve hex
digits, as utils/ci/.circleci/config.yml publishes it. The configs are
rendered on their way: __ZBWAKE_RELEASES__ becomes the list of gen 3 releases
of platform/diamond3/zbwake/releases, for the zbwake matrix.

    utils/ci/configs.py setup PARAMETERS CONFIG-LIST
        In the setup job: rewrites the parameters file the path-filtering orb
        wrote and the list of configs to merge.
    utils/ci/configs.py validate
        Merges every selection a pipeline can make (yq) and validates each
        with the circleci CLI.
"""
import hashlib
import json
import os
import subprocess
import sys
import tempfile

COMMON = ".circleci/common.yml"
RENDERED = "/tmp/circleci-configs"
ZBWAKE_RELEASES = "platform/diamond3/zbwake/releases"
NONE = ".circleci/none.yml"
DOCKERFILE = "utils/ci/Dockerfile"
IMAGE = "unfr0zen/unfr0zen-build"

# parameter: (its config, the subsystems whose jobs its workflow lists)
CONFIGS = {
    "build-image": ("utils/ci/.circleci/config.yml", []),
    "recovery": ("platform/diamond3/.circleci/config.yml", []),
    "zbwake": ("platform/diamond3/zbwake/.circleci/config.yml", []),
    "j49": ("platform/j49/.circleci/config.yml", []),
    "radio": ("platform/common/radio/.circleci/config.yml", []),
    "doom": ("platform/common/doom/.circleci/config.yml", []),
    "nlclient": ("platform/common/nlclient/.circleci/config.yml", []),
    "nestlocal": ("crates/nestlocal/.circleci/config.yml", ["radio", "zbwake", "j49"]),
    "app": ("crates/.circleci/config.yml", ["recovery", "zbwake", "j49", "radio", "doom", "nlclient", "nestlocal"]),
    "release": (".circleci/release.yml", ["app"]),
}


def configs_for(parameters):
    """The configs to merge for the parameters set: COMMON, each parameter's,
    and their dependencies' transitively; NONE when nothing is set."""
    selected, pending = set(), [name for name, on in parameters.items() if on is True and name in CONFIGS]
    while pending:
        name = pending.pop()
        if name in selected:
            continue
        selected.add(name)
        pending += CONFIGS[name][1]
    if not selected:
        return [NONE, COMMON]
    return [COMMON] + [CONFIGS[name][0] for name in CONFIGS if name in selected]


def render(configs, directory=RENDERED):
    """Copies of the configs with their placeholders filled, in directory."""
    with open(ZBWAKE_RELEASES) as f:
        releases = json.dumps(f.read().split())
    rendered = []
    for config in configs:
        with open(config) as f:
            text = f.read().replace("__ZBWAKE_RELEASES__", releases)
        path = os.path.join(directory, config.replace("/", "-"))
        os.makedirs(directory, exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        rendered.append(path)
    return rendered


def image_tag():
    with open(DOCKERFILE, "rb") as dockerfile:
        return IMAGE + ":" + hashlib.sha256(dockerfile.read()).hexdigest()[:12]


def setup(parameters_path, config_list_path):
    with open(parameters_path) as f:
        parameters = json.load(f)
    if os.environ.get("CIRCLE_TAG"):
        parameters = {"build-image": parameters.get("build-image", False), "release": True}
    parameters["image"] = image_tag()
    with open(parameters_path, "w") as f:
        json.dump(parameters, f)
    configs = configs_for(parameters)
    with open(config_list_path, "w") as f:
        f.writelines(config + "\n" for config in render(configs))
    print(json.dumps(parameters, indent=1))
    print("configs:", *configs)


def validate():
    selections = [{}] + [{name: True} for name in CONFIGS] + [{name: True for name in CONFIGS}]
    failed = False
    for parameters in selections:
        configs = configs_for(parameters)
        rendered = render(configs, tempfile.mkdtemp())
        merged = subprocess.run(["yq", "eval-all", "explode(.) | . as $item ireduce ({}; . * $item )", *rendered],
                                check=True, capture_output=True, text=True).stdout
        with tempfile.NamedTemporaryFile("w", suffix=".yml", delete=False) as f:
            f.write(merged)
        values = {**{name: True for name in parameters}, "image": IMAGE + ":000000000000"}
        result = subprocess.run(["circleci", "config", "process", "--pipeline-parameters", json.dumps(values), f.name],
                                capture_output=True, text=True)
        os.unlink(f.name)
        status = "ok" if result.returncode == 0 else "FAILED"
        failed |= result.returncode != 0
        print(f"{status}: {' '.join(sorted(parameters)) or 'nothing'} -> {len(configs)} configs")
        if result.returncode:
            print(result.stderr or result.stdout)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    if sys.argv[1:2] == ["setup"] and len(sys.argv) == 4:
        setup(sys.argv[2], sys.argv[3])
    elif sys.argv[1:] == ["validate"]:
        validate()
    else:
        sys.exit(__doc__)
