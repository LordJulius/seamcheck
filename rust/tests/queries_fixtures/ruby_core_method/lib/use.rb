class Use
  def go(opts, name)
    opts.each { |x| x }
    opts.key?(:a)
    name.to_s
    opts.custom_thing
  end
end
