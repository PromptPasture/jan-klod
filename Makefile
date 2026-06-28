.PHONY: all build ext run test lint wit clean

EXT_DIR := ext
SRC_DIR := src

all: ext build

build:
	go -C $(SRC_DIR) build -o ../bin/jan-klod ./cmd/jan-klod

ext: $(EXT_DIR)/store-memory.wasm $(EXT_DIR)/probe-host.wasm $(EXT_DIR)/provider-openai.wasm

# Build a guest extension to wasip1. $* is the extension name.
$(EXT_DIR)/%.wasm: $(SRC_DIR)/extensions/%/main.go $(SRC_DIR)/extensions/%/go.mod
	@mkdir -p $(EXT_DIR)
	GOOS=wasip1 GOARCH=wasm CGO_ENABLED=0 \
		go -C $(SRC_DIR)/extensions/$* build -buildmode=c-shared -o ../../../$(EXT_DIR)/$*.wasm .

run: all
	./bin/jan-klod

test: ext
	go -C $(SRC_DIR) test ./...

lint:
	cd $(SRC_DIR) && golangci-lint run

wit:
	wasm-tools component wit wit/

clean:
	rm -rf bin $(EXT_DIR)/*.wasm
