defmodule App.Session do
  def start(id, input) do
    maybe_inject(id, input)
    enqueue(id, input)
  end

  def other(id), do: enqueue(id, :x)
end
