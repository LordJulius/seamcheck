package cli

type flagSet struct{}

func (f *flagSet) Parse()         {}
func (f flagSet) String() string { return "" }

func helper(c *Command) error {
	x := &Command{}
	return x.Execute()
}
