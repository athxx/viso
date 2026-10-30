component Form {
    state saving = false

    view {
        Column {
            node title: Text { text: "Draft" }
            node save_button: Button { text: "Save" }
            if saving {
                node spinner: Spinner {}
            }
        }
    }
}
