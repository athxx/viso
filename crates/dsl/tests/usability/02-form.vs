export component SignUp {
    state name = "";
    state subscribed = false;
    state submitted = 0;

    computed ready: Bool = name != "";

    view {
        Column {
            gap: 8dp;
            TextInput { bind value <=> name; }
            CheckBox { bind checked <=> subscribed; }
            Text { text: name; }
            Button {
                text: "Sign up";
                on click {
                    if ready {
                        submitted += 1;
                    }
                }
            }
        }
    }
}
