//go:build ruleguard

// Package gorules holds the gocritic ruleguard rules of the go module, per ADR-0700.
package gorules

import "github.com/quasilyte/go-ruleguard/dsl"

// noIfInitAssign forbids the init-statement form of `if`. The three else shapes are written out, because gogrep has no wildcard for an else clause.
func noIfInitAssign(m dsl.Matcher) {
	m.Match(
		`if $*_ := $*_; $_ { $*_ }`,
		`if $*_ := $*_; $_ { $*_ } else { $*_ }`,
		`if $*_ := $*_; $_ { $*_ } else if $_ { $*_ }`,
		`if $*_ = $*_; $_ { $*_ }`,
		`if $*_ = $*_; $_ { $*_ } else { $*_ }`,
		`if $*_ = $*_; $_ { $*_ } else if $_ { $*_ }`,
	).Report(`no assignment in if-init: declare the variable on its own line`)
}

// errCompare forbids == and != on errors outside nil checks. After a %w wrap, they see only the outermost error.
// errorlint allows the sentinels that the standard library documents, and this rule does not.
func errCompare(m dsl.Matcher) {
	m.Match(
		`$x == $y`,
		`$x != $y`,
	).Where(
		m["x"].Type.Is(`error`) && m["y"].Type.Is(`error`) &&
			!m["x"].Text.Matches(`^nil$`) && !m["y"].Text.Matches(`^nil$`),
	).Report(`error compared with ==/!=; fails on wrapped errors, use errors.Is (or errors.As)`)

	m.Match(`switch $x { $*_ }`, `switch $*_; $x { $*_ }`).
		Where(m["x"].Type.Is(`error`)).
		Report(`switch on an error; fails on wrapped errors, use errors.Is in if/else if`)
}
