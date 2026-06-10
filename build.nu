def main [profile? : string] {

git submodule update --init --recursive
npx --yes tsx build/build.ts ($profile | default "quick")
}
