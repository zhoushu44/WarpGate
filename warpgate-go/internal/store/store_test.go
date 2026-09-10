package store

import (
	"path/filepath"
	"testing"
)

func TestMetaRoundTrip(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "warp-3")
	_ = t.TempDir()
	m := &Meta{
		Index: 3, Name: "warp-3", PID: 0, EnginePID: 0,
		Transport: "masque", IPv6: "2606:4700:110::1", Status: "running",
		CreatedAt: 1725926400000, Engine: "embedded",
	}
	if err := WriteMeta(dir, m); err != nil {
		t.Fatal(err)
	}
	got, err := ReadMeta(dir)
	if err != nil {
		t.Fatal(err)
	}
	if got.Index != 3 || got.IPv6 != m.IPv6 || got.Engine != "embedded" || got.CreatedAt != m.CreatedAt {
		t.Fatalf("meta 往返不一致: %+v", got)
	}
}

func TestReadMetaMissing(t *testing.T) {
	m, err := ReadMeta(filepath.Join(t.TempDir(), "warp-0"))
	if err != nil || m != nil {
		t.Fatalf("缺失 meta.json 应返回 nil,nil: %v %v", m, err)
	}
}

func TestDirName(t *testing.T) {
	if Dir("/root/warp-instances", 7) != filepath.Join("/root/warp-instances", "warp-7") {
		t.Fatal("目录名与 Node 版不一致")
	}
	if Name(7) != "warp-7" {
		t.Fatal("实例名与 Node 版不一致")
	}
}
