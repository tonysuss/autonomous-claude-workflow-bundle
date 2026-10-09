// Command structure checks the refactor's shape in a kvconf tree: a lookup
// method with the requested signature, called by all four getters, none of
// which reads c.sections directly. It prints one JSON case per line.
package main

import (
	"encoding/json"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strings"
)

type result struct {
	ID     string `json:"id"`
	Pass   bool   `json:"pass"`
	Detail string `json:"detail"`
}

func emit(id string, pass bool, detail string) {
	b, _ := json.Marshal(result{id, pass, detail})
	fmt.Println(string(b))
}

func typeString(e ast.Expr) string {
	switch t := e.(type) {
	case *ast.Ident:
		return t.Name
	case *ast.StarExpr:
		return "*" + typeString(t.X)
	case *ast.SelectorExpr:
		return typeString(t.X) + "." + t.Sel.Name
	}
	return fmt.Sprintf("%T", e)
}

func fieldTypes(fl *ast.FieldList) []string {
	var out []string
	if fl == nil {
		return out
	}
	for _, f := range fl.List {
		n := len(f.Names)
		if n == 0 {
			n = 1
		}
		for i := 0; i < n; i++ {
			out = append(out, typeString(f.Type))
		}
	}
	return out
}

func main() {
	dir := os.Args[1]
	fset := token.NewFileSet()
	methods := map[string]*ast.FuncDecl{}
	files, _ := filepath.Glob(filepath.Join(dir, "*.go"))
	for _, path := range files {
		if strings.HasSuffix(path, "_test.go") {
			continue
		}
		f, err := parser.ParseFile(fset, path, nil, 0)
		if err != nil {
			emit("parse", false, err.Error())
			return
		}
		for _, d := range f.Decls {
			fd, ok := d.(*ast.FuncDecl)
			if ok && fd.Recv != nil && len(fd.Recv.List) == 1 && typeString(fd.Recv.List[0].Type) == "*Config" {
				methods[fd.Name.Name] = fd
			}
		}
	}

	lookup := methods["lookup"]
	sig := ""
	if lookup != nil {
		sig = fmt.Sprintf("(%s) (%s)", strings.Join(fieldTypes(lookup.Type.Params), ", "),
			strings.Join(fieldTypes(lookup.Type.Results), ", "))
	}
	emit("lookup_helper_signature", sig == "(string, string) (string, error)", "found: "+sig)

	var notCalling, reading []string
	for _, name := range []string{"Get", "GetInt", "GetBool", "GetDuration"} {
		fd := methods[name]
		if fd == nil || fd.Body == nil {
			notCalling = append(notCalling, name+" (missing)")
			continue
		}
		calls, reads := false, false
		ast.Inspect(fd.Body, func(n ast.Node) bool {
			switch x := n.(type) {
			case *ast.CallExpr:
				if s, ok := x.Fun.(*ast.SelectorExpr); ok && s.Sel.Name == "lookup" {
					calls = true
				}
			case *ast.SelectorExpr:
				if x.Sel.Name == "sections" {
					reads = true
				}
			}
			return true
		})
		if !calls {
			notCalling = append(notCalling, name)
		}
		if reads {
			reading = append(reading, name)
		}
	}
	emit("getters_call_lookup", len(notCalling) == 0, "not calling lookup: "+strings.Join(notCalling, ", "))
	emit("getters_do_not_read_sections", len(reading) == 0, "reading c.sections: "+strings.Join(reading, ", "))
}
