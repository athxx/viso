import viso::time;

export component Search {
    state query = "";

    resource results: Resource<List<String>, String> {
        load = find(query);
        key = query;
        policy = [ResourcePolicy::keep_latest, ResourcePolicy::debounce(250ms)];
    }

    task find(q: String) -> Result<List<String>, String> {
        await time::sleep(100ms);
        if q == "" {
            return Ok([]);
        }
        Ok([q])
    }

    view {
        Column {
            gap: 8dp;
            TextInput { bind value <=> query; }
            match results.state {
                ResourceState::ready(items) => {
                    for item in items key item {
                        Text { text: item; }
                    }
                },
                ResourceState::error(message) => {
                    Text { text: message; }
                },
                _ => {
                    Text { text: "Searching"; }
                },
            }
        }
    }
}
