.PHONY: all build ext run test lint wit clean

EXT_DIR   := ext
SRC_DIR   := src
GUEST_DIR := src/extensions/store-memory

all: ext build

build:
	go -C $(SRC_DIR) build -o ../bin/jan-klod ./cmd/jan-klod

ext: $(EXT_DIR)/store-memory.wasm

$(EXT_DIR)/store-memory.wasm: $(GUEST_DIR)/main.go $(GUEST_DIR)/go.mod
	@mkdir -p $(EXT_DIR)
	GOOS=wasip1 GOARCH=wasm CGO_ENABLED=0 \
		go -C $(GUEST_DIR) build -buildmode=c-shared -o ../../../$(EXT_DIR)/store-memory.wasm .

run: all
	./bin/jan-klod

test:
	go -C $(SRC_DIR) test ./...

lint:
	cd $(SRC_DIR) && golangci-lint run

wit:
	wasm-tools component wit wit/

clean:
	rm -rf bin $(EXT_DIR)/*.wasm
