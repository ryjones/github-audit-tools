repos(){
  ./target/release/gh-enterprise-repos --visibility all --concurrency 10 -e $1 -o results/$1-all.yaml 
  ./target/release/gh-enterprise-repos --concurrency 10 -e $1 -o results/$1.yaml 
}
repos lf-decentralized-trust
repos openwallet-foundation
repos post-quantum-cryptography-alliance
