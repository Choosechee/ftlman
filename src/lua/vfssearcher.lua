local function vfssearcher(module)
	print("vfssearcher ran")
	return function() return {} end, nil
end
table.insert(package.searchers, vfssearcher)