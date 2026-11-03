require_relative "helpers"
require "sinatra/base"
def run_app
  helper_fn
  base_fn
  puts "x"
  Liquid::Template.parse("x")
  data = File.read("x")
  data.parse
end
