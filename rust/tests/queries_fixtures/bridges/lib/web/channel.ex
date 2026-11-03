defmodule Web.Channel do
  def handle_info(:tick, socket) do
    push(socket, "tick", %{})
  end
end
