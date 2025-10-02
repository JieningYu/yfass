# YFASS

**Y**et-another-**F**unction-**AS**-a-**S**ervice-Platform.

_But why not YFaaS?_

## Features

- Deploy multiple services with versioning and alias support
- Isolate host environment for each service while maintaining network access
- Proxy **HTTP** and **Websocket** connections between clients and services
- Configurable emulated environment for each service
- HTTP API for service management
- Basic account system with authorization

## Platform Support

Currently the only supported platform is GNU/Linux. The sandbox is implemented using [**bubblewrap**](https://github.com/containers/bubblewrap) with optional `seccomp` support for filtering system calls.

## Dependencies

### GNU/Linux

- **bwrap**: bubblewrap is required on runtime.
- **libseccomp**: used to compile BPF filters and is required when feature `seccomp` is enabled. Devel package is required for building.

## Configuration

There are two kinds of configuration: for the platform and for services.
The former one is done by passing command line arguments to the `yfass` executable which you could check it out by `--help`.
The latter one should be configured at runtime of platform through its API (will be stored persistently though).

### Example configuration of a service

```jsonc
{
  // note this is jsonc but in practice only json is supported. watch out!

  // User group to manage this service.
  // By default this will be the person who created the service.
  // But at last root user has full access to everything.
  "group": "permission:admin",

  // Socket address the underlying service will listen on.
  // This is used for forwarding connections to the service.
  //
  // In this case I'm using port 25565 and as you can see it's the
  // default port for Minecraft. But you can't run a Minecraft server
  // on a faas platform as writes on filesystem are forbidden.
  "addr": "127.0.0.1:25565",

  "sandbox": {
    // Path to the executable, relative to the `contents` directory.
    // Don't miss the `./` prefix or it won't work.
    "command": "./test-ws-gzip-fn",
    // Arguments passed to the executable.
    "args": [],
    // Read-only filesystem bindings. The key is the path in host and
    // the value is the path in sandbox.
    "ro_entries": {
      // lib and lib64 are necessarily required for most binaries as
      // they contain the dynamic linker.
      "/lib64": "/lib64",
      "/lib": "/lib"
    },
    // Environment variables passed to the executable.
    "envs": {
      // The service I'm running can configure itself to listen on a
      // port depending on the environment variable `YFASS_PORT`.
      // But it's no relation to the faas platform.
      "YFASS_PORT": "25565"
    },
    // Whether to inherit stdout and stderr from the the host.
    "inherit_stdout": true,

    // Linux-only configuration
    // (but we don't support other platforms yet)

    // Mode to filter system calls. Can be either `Allow` or `Deny`.
    "syscall_filter_mode": "Deny",
    // List of system call names to filter. Here we block `fork`.
    // And if you changed the mode to `Allow` then `fork` is the only
    // allowed system call. How cool is that?
    "syscall_filter": ["fork"],

    // Linux filesystem mounts
    "mount_procfs": true,
    "mount_devtmpfs": true,
    "mount_tmpfs": false
  }
}
```

## API

docs: TBD...

Check the source code for now. They have some inline docs for you to check.

## Access to functions

Access to functions is done through HTTP or Websocket and specifying which function you are trying to access is done by host name resolution.

For example, if the platform is hosted on `example.com` and you have a function named `test` with version `a0` then you can access it through `a0.test.example.com`.
Technically this is done by parsing the host header in HTTP requests so keep an eye if you are walking into any problem related to that.

## Project Report

```rust
#![doc(hidden)]
```

### Usage of LLM for codegen

`qwen3-coder` is used for most fundamental code completion of template codes and inline docs.

There's no other usage of LLM for codegen.

### Design

Essential features given by requirements are as follow:

1. Providing HTTP and Websocket support for connecting functions, and resolve hostname to corresponding function socket addresses
2. Deployment APIs as well as management APIs
3. Multi-function support
4. Network access of functions
5. Persist functions and metadata
6. Multi-version support
7. Authorization
8. Zero-downtime deployment
9. Environment variable passthrough
10. Runtime toolchain support
11. Low resource consumption
12. High performance both ahead-of-time and just-in-time
13. Isolation of functions from host environment

To build features above altogether what we need are basically:

1. **Flexibility** to support different toolchains and language runtimes
2. **HTTP API** to manage the platform
3. **User system** to do authorization stuff
4. **Lightweight Sandbox** to isolate functions from host environment while maintaining flexibility and low resource consumption
5. **Data Storage** to persist functions and users
6. **Forwarding** HTTP and Websocket connections to functions and vise versa
7. **Treat different versions of a function** as completely different functions

And to achieve the key points above, we have to pick best-matched technologies and solutions:

- **Rust** as the programming language. It's high performance, and if needed, could be verbose to control all the details while fulfilling my development experience.
- **Axum** to build HTTP services and the proxy which forwards HTTP requests from subdomains to function addresses. The only reason to use it is that I have used it before and there's no significant drawback of it.
- **Tungstenite** for Websocket support.
- **Bubblewrap** for sandbox implementation. It's already used by `Flatpak` thus is secure in practice and enough lightweight for function isolation. About using native binaries over thing like Webassembly I assume that it will be another great but tough story to build up runtime libraries (like JRE) in a WASM environment.
- **Store data directly in the FS.** We don't need a database to mess things up.

There we have our architecture well-confirmed.

#### Platform Abstraction

While our target platform is GNU/Linux we still need to reserve the possibility to port this into other platforms such as macOS. So here for our platform-dependent code we surely need abstractions:

```rust
#[cfg(target_os = "linux")]
type __SandboxImpl = linux::Bubblewrap;

#[cfg(not(target_os = "linux"))]
type __SandboxImpl = Unimplemented;

#[cfg(target_os = "linux")]
type SandboxConfigExt = crate::os::linux::SandboxConfigExt;

#[cfg(not(target_os = "linux"))]
type SandboxConfigExt = SandboxConfigExtFallback;
```

#### Proxy Implementation

We maintain a blazing fast hash index of the hostname prefixes to the functions' socket addresses they are listening on. In practice it is actually an URI authority for stripping the address-to-string conversion overhead.

When a request comes in, we look up the hostname prefix in the index and then compare the hostname against the prefix. If the hostname matches, we know that the request is for one single function.

Here we split a request into two cases:

##### Normal HTTP request

Forwarded to the function address by sending HTTP request to it through client provided by `hyper-util`. `reqwest` is somehow bloated so I don't want to even touch it (although used by a test client).

##### WebSocket connection request

Parsed the upgrade request by Axum, then forward the connection request to function using `tokio-tungstenite` so we technically got two Websocket connections that are `client <-> server` and `server <-> function`. Now we establish two tokio tasks in the server:

1. Receive messages from the client, and send them to the function.
2. Receive messages from the function, and send them to the client.

This approach has been tested with `ws-gzip` test case in this repo.

#### Bubblewrap Setup

Theoretically a spawned function should not have access to the host's filesystem. But in practice it is fine to share a small set of read-only files that are necessarily required for the function to run, which includes the dynamic linker, shared libraries, JRE if you are running Java, and so on.

The function's main directory is lied by the directory structure uploaded to the platform, whose former one is in `/.__private_yfass_contents` directory in the sandbox environment.

Read-only entries above are open to be configured per-function through configuration files.

#### User Groups

User groups are tags attached to a arbitrary user for identifying permissions and custom categorization.

A user group could be either `permission:<permission name>` or `custom:<name>`. A set of special group exists for identity of users like `singular:yjn024` for the only user that is myself.

### Issues during Development

#### IPC through Unix Pipe FD

`bwrap` accepts BPF Filter through passing a FD id to its arguments. At first I didn't have the concept of per-process File Descriptor mappings and so ownerships so I just passed the ID of read FD in parent process to bubblewrap's arguments. But bubblewrap complains that the FD is invalid. Somehow I now claim its ridiculous to have no ownership of File Descriptors as this will break permission control.

After learning the existence of per-process FD tables I got started to work on passing ownership of read FD to bubblewrap. I asked a LLM but while it gave me a solution to call unsafe function `pre_exec` of `CommandExt` trait to do bare syscalls for cloning the FD into subprocess. This is way too verbose and I'm not that confident to do unsafe stuff across processes.

And that I looked into GitHub and found a crate `command-fds` made by Google which can do all the things up packed into a function. Well a perfect solution which actually works.

#### Wrongly-used Axum newtype in trait implementation

In Axum we use `axum::extract::State` for stateful injection but it should only be used as a service extractor. However I used that newtype wrapper in a trait implementation of `FromRequestParts<S>` for extractor type `Auth<const P>` where `S` is the state type. So it's a wrong usage thus makes the compiler to complain that functions receiving `Auth` are not valid service handlers.

Rust compiler is not smart enough to figure out that `Auth` is not a service extractor matching single-newtype wrapper. Instead it is a doubly-newtype stateful extractor. I checked through the codebase and Axum docs and finally after burden of hours found out the real issue. Have to laugh myself.

#### Crappy bubblewrap arguments

At first I mounted `./` from the host filesystem (read-only) to `./` in the sandbox. I used to assume that `./` in the sandbox is identical to `./` in the host. But it's not, instead, it means `/` thus I can't continue to bind any other directory to `/` like `/lib`.

I realized this by endless tries of invoking `ls` and `pwd` in command line using bubblewrap. So after that I switched to bind into `/.__private_yfass_contents` in sandbox. And then everthing works.

### Pros and Cons

#### Pros

- Low resource usage: What truly matters compared with bare-execution are the platform backend itself written in Rust which uses nothing and bubblewrap which is a rootless tiny wrapper.
- Full access to host-installed libraries and runtimes if permitted.
- Each function can be configured with a JSON file.
- Management of the platform could be done through HTTP APIs at all scale.

#### Cons

- Minimum tests which lack corner cases.
- No documented APIs.
- Only supports GNU/Linux.
- Hard to use without a client. (I used Firefox devtools to shoot requests)
- Forwards network traffic which impacts performance. (However necessary for now as all subdomains of the main host are routed to the platform)
- No auto-restart and auto-launch of functions. (But easy to implement though through watchdog)
- Functions need to care about what libraries are installed on the host or they have to carry their own.

### Journey

At the time I received the mail from Team BYR, which was in midnight, I built the architecture of the platform in mind on the bed. That night I was stimulated by the idea of the platform.

So the days followed I started coding at free time. While overall I enjoyed the process, except writing APIs and tests. But well there's no significant changes in my emotions.

But the pain starts when I learned that I have to write this report. I'm not a native English speaker and not capable to write a Chinese report as well due to poor expression capabilities. But as English is not my mother language I could express in a way more boundlessly. However, still, I'm hard to write a report. So sorry for this.
