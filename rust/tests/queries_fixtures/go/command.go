package cli

import (
	"fmt"

	"github.com/acme/cli/internal/render"
	ui "github.com/acme/cli/tui"
)

type Command struct {
	flags *flagSet
	win   ui.Window
}

func (c *Command) Execute() error {
	_, err := c.ExecuteC()
	fmt.Println(err)
	c.flags.Parse()
	c.win.Draw()
	render.Page(c.String())
	return helper(c)
}

func (c *Command) ExecuteC() (*Command, error) {
	return c, nil
}

func (c Command) String() string { return "" }
