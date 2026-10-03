package main

import (
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"
	"time"
)

func TestStandbyAndPlan(t *testing.T) {
	if Standby(Blue) != Green || Standby(Green) != Blue {
		t.Fatal("standby must be the other slot")
	}
	u := Plan(Blue)
	if u.From != Blue || u.To != Green {
		t.Fatalf("plan from blue should target green, got %+v", u)
	}
}

func TestResolveAndAfterHealth(t *testing.T) {
	u := Plan(Blue) // from=blue, to=green
	if u.Resolve(true) != Green {
		t.Fatal("healthy resolve should keep the new slot")
	}
	if u.Resolve(false) != Blue {
		t.Fatal("unhealthy resolve should roll back")
	}

	base := State{Active: Blue, Previous: Green, Version: "1"}
	ok := base.AfterHealth(u, true, "2")
	if ok.Active != Green || ok.Previous != Blue || ok.Version != "2" {
		t.Fatalf("healthy commit wrong: %+v", ok)
	}
	bad := base.AfterHealth(u, false, "2")
	if bad.Active != Blue || bad.Version != "1" {
		t.Fatalf("failed update must not change the active slot: %+v", bad)
	}
}

func TestStateLoadDefaultAndRoundTrip(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "state.json")

	// Missing file defaults to blue-active.
	got, err := LoadState(path)
	if err != nil || got.Active != Blue {
		t.Fatalf("default state wrong: %+v err=%v", got, err)
	}

	want := State{Active: Green, Previous: Blue, Version: "0.1.0"}
	if err := want.Save(path); err != nil {
		t.Fatal(err)
	}
	back, err := LoadState(path)
	if err != nil || back != want {
		t.Fatalf("round-trip wrong: %+v err=%v", back, err)
	}
}

func TestActivateRoundTrip(t *testing.T) {
	root := t.TempDir()
	if err := Activate(root, Blue); err != nil {
		t.Fatal(err)
	}
	if slot, _ := ActiveSlot(root); slot != Blue {
		t.Fatalf("expected blue active, got %s", slot)
	}
	// Flipping replaces the link (atomic rename over an existing symlink).
	if err := Activate(root, Green); err != nil {
		t.Fatal(err)
	}
	if slot, _ := ActiveSlot(root); slot != Green {
		t.Fatalf("expected green active after flip, got %s", slot)
	}
}

func TestProbe(t *testing.T) {
	ok := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	}))
	defer ok.Close()
	if !Probe(ok.URL, time.Second) {
		t.Fatal("200 should probe healthy")
	}

	bad := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer bad.Close()
	if Probe(bad.URL, time.Second) {
		t.Fatal("500 should probe unhealthy")
	}
	if Probe("http://127.0.0.1:1", 200*time.Millisecond) {
		t.Fatal("unreachable should probe unhealthy")
	}
}

func TestPromoteCommitsOnHealthy(t *testing.T) {
	root := t.TempDir()
	stateFile := filepath.Join(root, "state.json")
	_ = Activate(root, Blue)

	started := Slot("")
	stopped := false
	start := func(s Slot) (func(), error) { started = s; return func() { stopped = true }, nil }

	state, err := Promote(root, stateFile, "2.0", start, func() bool { return true })
	if err != nil {
		t.Fatal(err)
	}
	if started != Green {
		t.Fatalf("should start the staged (green) slot, started %s", started)
	}
	if stopped {
		t.Fatal("a healthy promotion must not stop the new core")
	}
	if state.Active != Green || state.Version != "2.0" {
		t.Fatalf("healthy promote should commit green@2.0, got %+v", state)
	}
	if slot, _ := ActiveSlot(root); slot != Green {
		t.Fatalf("symlink should point at green, got %s", slot)
	}
}

func TestPromoteRollsBackOnUnhealthy(t *testing.T) {
	root := t.TempDir()
	stateFile := filepath.Join(root, "state.json")
	_ = Activate(root, Blue)

	stopped := false
	start := func(_ Slot) (func(), error) { return func() { stopped = true }, nil }

	state, err := Promote(root, stateFile, "2.0", start, func() bool { return false })
	if err != nil {
		t.Fatal(err)
	}
	if !stopped {
		t.Fatal("a failed promotion must stop the bad core")
	}
	if state.Active != Blue {
		t.Fatalf("failed promote should keep blue active, got %+v", state)
	}
	if slot, _ := ActiveSlot(root); slot != Blue {
		t.Fatalf("symlink should be rolled back to blue, got %s", slot)
	}
}
