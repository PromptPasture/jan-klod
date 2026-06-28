// Package abi defines the calling convention between the core host and WASM
// extensions. Extensions are core WASM modules (not yet component-model
// components, which wazero does not support). Requests and responses are
// JSON payloads passed through guest linear memory.
//
// A guest exports:
//
//	alloc(size u32) -> ptr u32
//	free(ptr u32)
//	invoke(ptr u32, len u32) -> u64   // returns Pack(resultPtr, resultLen)
package abi

// Pack folds a pointer and length into a single u64 return value
// (pointer in the high 32 bits, length in the low 32 bits).
func Pack(ptr, length uint32) uint64 {
	return uint64(ptr)<<32 | uint64(length)
}

// Unpack splits a u64 produced by Pack back into pointer and length.
func Unpack(v uint64) (ptr, length uint32) {
	return uint32(v >> 32), uint32(v)
}
