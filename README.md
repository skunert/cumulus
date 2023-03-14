# Cumulus ☁️

[![Doc](https://img.shields.io/badge/cumulus%20docs-master-brightgreen)](https://paritytech.github.io/cumulus/)

This repository contains both the Cumulus SDK and also specific chains implemented
on top of this SDK.

## Cumulus SDK

A set of tools for writing [Substrate](https://substrate.io/)-based
[Polkadot](https://wiki.polkadot.network/en/)
[parachains](https://wiki.polkadot.network/docs/en/learn-parachains). Refer to the included
[overview](docs/overview.md) for architectural details, and the
[Connect to relay and  parachain tutorials](https://docs.substrate.io/tutorials/connect-relay-and-parachains/) for a
guided walk-through of using these tools.

It's easy to write blockchains using Substrate, and the overhead of writing parachains'
distribution, p2p, database, and synchronization layers should be just as low. This project aims to
make it easy to write parachains for Polkadot by leveraging the power of Substrate.

Cumulus clouds are shaped sort of like dots; together they form a system that is intricate,
beautiful and functional.

### Consensus

[`parachain-consensus`](https://github.com/paritytech/cumulus/blob/master/client/consensus/common/src/parachain_consensus.rs) is a
[consensus engine](https://docs.substrate.io/v3/advanced/consensus) for Substrate
that follows a Polkadot
[relay chain](https://wiki.polkadot.network/docs/en/learn-architecture#relay-chain). This will run
a Polkadot node internally, and dictate to the client and synchronization algorithms which chain
to follow,
[finalize](https://wiki.polkadot.network/docs/en/learn-consensus#probabilistic-vs-provable-finality),
and treat as best.

### Collator

A Polkadot [collator](https://wiki.polkadot.network/docs/en/learn-collator) for the parachain is
implemented by the `polkadot-parachain` binary (previously called `polkadot-collator`).

### Relaychain Interaction
To operate a parachain node a connection to the corresponding relaychain is necessary. This can be achieved in one of two ways:
1. Run a full relaychain node within the parachain node (default)
2. Connect to an external relaychain node via websocket RPC

#### In-process Relaychain Node
By default if an external relaychain node is not specified then a full relaychain node will be spawned within the same process.

This node has all of the typical components of a normal Polkadot node, and will have to fully sync with the relaychain to work.

##### Example command
```shell=
#                                                          In-process node with this chainspec is spawned
#                                                                                |
#                                                                    |-----------------------|
polkadot-parachain --chain parachain-chainspec.json --tmp -- --chain relaychain-chainspec.json
```

#### External Relaychain Node
You can connect to an external relaychain node via websocket RPC by using the  `--relay-chain-rpc-urls` command line argument. This option accepts one or more space-separated websocket URLs to a full relay chain node. By default only the first URL will be used, with the rest acting as a backup in case the connection to the first node will be lost.

Parachain nodes using this feature won't have to fully sync with the relay chain to work, so in general they will use significantly less system resources.

**Note:** At this time any parachain nodes using this feature will still spawn a very cut down relaychain node in-process, hence even though they lack the majority of normal Polkadot subsystems they will still need to be able to directly connect to the relay chain network.
##### Example command
```shell=
#                                                                                    Perform runtime calls and fetch                                                   Still required since we connect
#                                                                                    data via RPC from here.                     Backup node                           to the relaychain network
#                                                                                              |                                      |                                           |
#                                                                                |-------------------------------| |--------------------------------------|            |-----------------------|
polkadot-parachain --chain parachain-chainspec.json --tmp --relay-chain-rpc-urls ws://relaychain-rpc-endpoint:9944 ws://relaychain-rpc-endpoint-backup:9944 -- --chain relaychain-chainspec.json
```

## Installation and Setup
Before building Cumulus SDK based nodes / runtimes prepare your environment by following Substrate [installation instructions](https://docs.substrate.io/main-docs/install/).

To launch a local network, you can use [zombienet](https://github.com/paritytech/zombienet) for quick setup and experimentation or follow the [manual setup](#manual-setup).

### Zombienet
We use Zombienet to spin up networks for integration tests and local networks. Follow [these installation steps](https://github.com/paritytech/zombienet#requirements-by-provider) to set it up on your machine.
A simple network specification with two relay chain nodes and one collator is located at [zombienet/examples/small_network.toml](zombienet/examples/small_network.toml).


#### Which provider should I use?
Zombienet offers multiple providers to run networks. Choose the one that best fits your needs:
- **Podman:** Choose this if you want to spin up a network quick and easy.
- **Native:** Choose this if you want to develop and deploy your changes. Requires compilation of the binaries.
- **Kubernetes:** Choose this for advanced use-cases or running on cloud-infrastructure.

#### How to run
To run the example network, use the following commands:

```bash
# Podman provider
zombienet --provider podman spawn ./zombienet/examples/small_network.toml

# Native provider, assumes polkadot and polkadot-parachains binary in $PATH
zombienet --provider native spawn ./zombienet/examples/small_network.toml
```

### Manual Setup
#### Launch the Relay Chain

```bash
# Clone
git clone https://github.com/paritytech/polkadot
cd polkadot

# Compile Polkadot with the real overseer feature
cargo build --release --bin polkadot

# Generate a raw chain spec
./target/release/polkadot build-spec --chain rococo-local --disable-default-bootnode --raw > rococo-local-cfde.json

# Alice
./target/release/polkadot --chain rococo-local-cfde.json --alice --tmp

# Bob (In a separate terminal)
./target/release/polkadot --chain rococo-local-cfde.json --bob --tmp --port 30334
```

#### Launch the Parachain

```bash
# Clone
git clone https://github.com/paritytech/cumulus
cd cumulus

# Compile
cargo build --release --bin polkadot-parachain

# Export genesis state
./target/release/polkadot-parachain export-genesis-state > genesis-state

# Export genesis wasm
./target/release/polkadot-parachain export-genesis-wasm > genesis-wasm

# Collator1
./target/release/polkadot-parachain --collator --alice --force-authoring --tmp --port 40335 --ws-port 9946 -- --execution wasm --chain ../polkadot/rococo-local-cfde.json --port 30335

# Collator2
./target/release/polkadot-parachain --collator --bob --force-authoring --tmp --port 40336 --ws-port 9947 -- --execution wasm --chain ../polkadot/rococo-local-cfde.json --port 30336

# Parachain Full Node 1
./target/release/polkadot-parachain --tmp --port 40337 --ws-port 9948 -- --execution wasm --chain ../polkadot/rococo-local-cfde.json --port 30337
```

#### Register the parachain

![image](https://user-images.githubusercontent.com/2915325/99548884-1be13580-2987-11eb-9a8b-20be658d34f9.png)


## Statemint 🪙

This repository also contains the Statemint runtime (as well as the canary runtime Statemine and the
test runtime Westmint).
Statemint is a system parachain providing an asset store for the Polkadot ecosystem.

### Build & Launch a Node

To run a Statemine or Westmint node (Statemint is not deployed, yet) you will need to compile the
`polkadot-parachain` binary:

```bash
cargo build --release --locked -p polkadot-parachain
```

Once the executable is built, launch the parachain node via:

```bash
CHAIN=westmint # or statemine
./target/release/polkadot-parachain --chain $CHAIN
```

Refer to the [setup instructions](#local-setup) to run a local network for development.

## Contracts 📝

See [the `contracts-rococo` readme](parachains/runtimes/contracts/contracts-rococo/README.md) for details.

## Bridge-hub 📝

See [the `bridge-hubs` readme](parachains/runtimes/bridge-hubs/README.md) for details.

## Rococo 👑

[Rococo](https://polkadot.js.org/apps/?rpc=wss://rococo-rpc.polkadot.io) is becoming a [Community Parachain Testbed](https://polkadot.network/blog/rococo-revamp-becoming-a-community-parachain-testbed/) for parachain teams in the Polkadot ecosystem. It supports multiple parachains with the differentiation of long-term connections and recurring short-term connections, to see which parachains are currently connected and how long they will be connected for [see here](https://polkadot.js.org/apps/?rpc=wss%3A%2F%2Frococo-rpc.polkadot.io#/parachains).

Rococo is an elaborate style of design and the name describes the painstaking effort that has gone
into this project.

### Build & Launch Rococo Collators

Collators are similar to validators in the relay chain. These nodes build the blocks that will
eventually be included by the relay chain for a parachain.

To run a Rococo collator you will need to compile the following binary:

```bash
cargo build --release --locked -p polkadot-parachain
```

Otherwise you can compile it with
[Parity CI docker image](https://github.com/paritytech/scripts/tree/master/dockerfiles/ci-linux):

```bash
docker run --rm -it -w /shellhere/cumulus \
                    -v $(pwd):/shellhere/cumulus \
                    paritytech/ci-linux:production cargo build --release --locked -p polkadot-parachain
sudo chown -R $(id -u):$(id -g) target/
```

If you want to reproduce other steps of CI process you can use the following
[guide](https://github.com/paritytech/scripts#gitlab-ci-for-building-docker-images).

Once the executable is built, launch collators for each parachain (repeat once each for chain
`tick`, `trick`, `track`):

```bash
./target/release/polkadot-parachain --chain $CHAIN --validator
```

### Parachains

* [Statemint](https://polkadot.js.org/apps/?rpc=wss%3A%2F%2Frococo-statemint-rpc.polkadot.io#/explorer)
* [Contracts on Rococo](https://polkadot.js.org/apps/?rpc=wss%3A%2F%2Frococo-contracts-rpc.polkadot.io#/explorer)
* [RILT](https://polkadot.js.org/apps/?rpc=wss%3A%2F%2Frococo.kilt.io#/explorer)

The network uses horizontal message passing (HRMP) to enable communication between parachains and
the relay chain and, in turn, between parachains. This means that every message is sent to the relay
chain, and from the relay chain to its destination parachain.

### Containerize

After building `polkadot-parachain` with cargo or with Parity CI image as documented in [this chapter](#build--launch-rococo-collators),
the following will allow producing a new docker image where the compiled binary is injected:

```bash
./docker/scripts/build-injected-image.sh
```

Alternatively, you can build an image with a builder pattern:

```bash
docker build --tag $OWNER/$IMAGE_NAME --file ./docker/polkadot-parachain_builder.Containerfile .

You may then run your new container:

```bash
docker run --rm -it $OWNER/$IMAGE_NAME --collator --tmp --execution wasm --chain /specs/westmint.json
```
