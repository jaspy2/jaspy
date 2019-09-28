#!/bin/bash -e
current_dir="${PWD}"
build_target="${PWD}/output"
version="$(git describe --tags | grep -oE '[0-9].+')"

built_components=""

# buildable components
buildable_components="nexus poller pinger snmptrapd-reader discover snmpbot entitypoller switchmaster"
for build_component in ${buildable_components}; do
    pushd "${current_dir}/../${build_component}"
    docker build -f Dockerfile --target builder -t "jaspy/${build_component}_builder:${version}" .
    built_components="${built_components} ${build_component}"
    popd
done

for component in ${built_components}; do
    component_target="${build_target}/${component}"
    mkdir -p "${component_target}"
    rm -rf "${component_target}/" 2>/dev/null || true
    docker run --rm -v "${component_target}:/output" "jaspy/${component}_builder:${version}"
done

# cli
rm -rf "${build_target}/cli"
cp -a "${current_dir}/../cli" "${build_target}/cli"

# weathermap
rm -rf "${build_target}/weathermap"
cp -a "${current_dir}/../weathermap" "${build_target}/weathermap"

# debian
rm -rf "${build_target}/debian"
docker build -f debian/Dockerfile -t "jaspy/deb:${version}" --build-arg version="${version}" .
docker run --rm -v "${build_target}/debian:/output" "jaspy/deb:${version}"
