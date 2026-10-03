# jan-klod-supervisor

The blue/green updater: stages a new core and its extensions, flips the active slot, health-checks, and rolls back on failure. It is a separate process from the core on purpose, so the component doing the swap is never the binary being swapped. A Go program that depends on the standard library only.

```sh
cd pkgs/host/supervisor && go test ./...
go run . status            # or: promote
```

Slot layout and update flow: [docs/blue-green-deployment.md](docs/blue-green-deployment.md).
