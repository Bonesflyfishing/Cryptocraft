remote: Compressing objects: 100% (4/4), done.
remote: Total 4 (delta 3), reused 0 (delta 0), pack-reused 0 (from 0)
Unpacking objects: 100% (4/4), 1.01 KiB | 35.00 KiB/s, done.
From https://github.com/Bonesflyfishing/Cryptocraft
   1a592ad..a58d1d7  main       -> origin/main
Updating 1a592ad..a58d1d7
Fast-forward
 src/blockchain.rs | 4 ++--
 1 file changed, 2 insertions(+), 2 deletions(-)
chiefmegavolt@penguin:~/Cryptocraft$ cargo build
   Compiling cryptocraft v0.1.0 (/home/chiefmegavolt/Cryptocraft)
error[E0432]: unresolved imports `crate::pool_server::DISCOVERY_PING`, `crate::pool_server::DISCOVERY_PONG`, `crate::pool_server::DISCOVERY_PORT`
 --> src/network.rs:5:26
  |
5 | use crate::pool_server::{DISCOVERY_PING, DISCOVERY_PONG, DISCOVERY_PORT};
  |                          ^^^^^^^^^^^^^^  ^^^^^^^^^^^^^^  ^^^^^^^^^^^^^^ no `DISCOVERY_PORT` in `pool_server`
  |                          |               |
  |                          |               no `DISCOVERY_PONG` in `pool_server`
  |                          no `DISCOVERY_PING` in `pool_server`

error[E0432]: unresolved imports `crate::pool_server::ClientMsg`, `crate::pool_server::ServerMsg`
  --> src/pool_client.rs:11:26
   |
11 | use crate::pool_server::{ClientMsg, ServerMsg};
   |                          ^^^^^^^^^  ^^^^^^^^^ no `ServerMsg` in `pool_server`
   |                          |
   |                          no `ClientMsg` in `pool_server`
   |                          help: a similar name exists in the module: `ClientUi`

error[E0432]: unresolved imports `crate::pool_server::ClientMsg`, `crate::pool_server::ServerMsg`
  --> src/pool_server.rs:11:26
   |
11 | use crate::pool_server::{ClientMsg, ServerMsg};
   |                          ^^^^^^^^^  ^^^^^^^^^ no `ServerMsg` in `pool_server`
   |                          |
   |                          no `ClientMsg` in `pool_server`
   |                          help: a similar name exists in the module: `ClientUi`

error[E0425]: cannot find value `POOL_PORT` in module `pool_server`
   --> src/main.rs:192:76
    |
192 |                 println!("  Bound to    : {}:{}", display_ip, pool_server::POOL_PORT);
    |                                                                            ^^^^^^^^^ not found in `pool_server`

error[E0425]: cannot find value `POOL_PORT` in module `pool_server`
   --> src/main.rs:193:76
    |
193 |                 println!("  Clients use : {}:{}", display_ip, pool_server::POOL_PORT);
    |                                                                            ^^^^^^^^^ not found in `pool_server`

error[E0425]: cannot find value `POOL_PORT` in module `pool_server`
   --> src/main.rs:204:77
    |
204 |                 let server_addr = network::pick_server_address(pool_server::POOL_PORT);
    |                                                                             ^^^^^^^^^ not found in `pool_server`

warning: unused imports: `io::Cursor` and `net::TcpListener`
  --> src/server.rs:13:5
   |
13 |     io::Cursor,
   |     ^^^^^^^^^^
14 |     net::TcpListener,
   |     ^^^^^^^^^^^^^^^^
   |
   = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

warning: unused import: `Stylize`
  --> src/main.rs:10:81
   |
10 | use crossterm::{cursor, execute, style::{Color, ResetColor, SetForegroundColor, Stylize}, terminal::{self, ClearType}};
   |                                                                                 ^^^^^^^

warning: unused import: `Sha256`
  --> src/main.rs:13:20
   |
13 | use sha2::{Digest, Sha256};
   |                    ^^^^^^

warning: unused imports: `SystemTime`, `UNIX_EPOCH`, `fs`, and `path::Path`
  --> src/main.rs:16:5
   |
16 |     fs,
   |     ^^
17 |     io::{self, BufRead, Write},
18 |     path::Path,
   |     ^^^^^^^^^^
...
23 |     time::{Duration, Instant, SystemTime, UNIX_EPOCH},
   |                               ^^^^^^^^^^  ^^^^^^^^^^

warning: unused imports: `Deserialize` and `Serialize`
  --> src/main.rs:25:13
   |
25 | use serde::{Deserialize, Serialize};
   |             ^^^^^^^^^^^  ^^^^^^^^^

error[E0308]: mismatched types
   --> src/main.rs:198:34
    |
198 |                 pool_server::run(blockchain, session.chain_file.clone(), bind_ip);
    |                 ---------------- ^^^^^^^^^^ expected `String`, found `Blockchain`
    |                 |
    |                 arguments to this function are incorrect
    |
