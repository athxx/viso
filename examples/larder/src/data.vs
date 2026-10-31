// The catalog the app starts from: what the recipes resource loads, and the
// shopping list and plan a new kitchen begins with.

import model::{Recipe, Ingredient, Step, Course, Measure, Aisle, ShoppingItem, PlannedMeal, Weekday}

fn item(name: String, amount: F64, unit: Measure, aisle: Aisle) -> Ingredient {
    Ingredient { name: name, amount: amount, unit: unit, aisle: aisle }
}

fn step(text: String) -> Step {
    Step { text: text }
}

fn timed(text: String, minutes: I64) -> Step {
    Step { text: text, minutes: minutes }
}

export fn catalog() -> List<Recipe> {
    [
        Recipe {
            id: 1
            title: "Shakshuka"
            summary: "Eggs poached in a spiced tomato and pepper sauce, eaten from the pan with bread."
            course: Course::breakfast
            minutes: 30
            serves: 2
            picture: asset("dishes/shakshuka.svg")
            tags: ["eggs", "vegetarian", "one pan"]
            ingredients: [
                item("Olive oil", 2.0, Measure::spoon, Aisle::pantry),
                item("Onion", 1.0, Measure::piece, Aisle::produce),
                item("Red pepper", 1.0, Measure::piece, Aisle::produce),
                item("Garlic clove", 2.0, Measure::piece, Aisle::produce),
                item("Cumin", 1.0, Measure::pinch, Aisle::spices),
                item("Smoked paprika", 1.0, Measure::pinch, Aisle::spices),
                item("Chopped tomatoes", 400.0, Measure::gram, Aisle::pantry),
                item("Eggs", 4.0, Measure::piece, Aisle::dairy),
                item("Feta", 50.0, Measure::gram, Aisle::dairy),
                item("Flatbread", 2.0, Measure::piece, Aisle::bakery),
            ]
            steps: [
                timed("Soften the onion and pepper in the oil over a medium heat.", 8),
                step("Add the garlic and spices and stir for a minute until fragrant."),
                timed("Pour in the tomatoes, season, and simmer until thick.", 10),
                timed("Make four wells, crack in the eggs, cover and cook until the whites set.", 6),
                step("Crumble over the feta and serve from the pan with warm bread."),
            ]
        },
        Recipe {
            id: 2
            title: "Overnight oats"
            summary: "Oats soaked in milk and yoghurt overnight, topped with fruit in the morning."
            course: Course::breakfast
            minutes: 5
            serves: 1
            picture: asset("dishes/oats.svg")
            tags: ["make ahead", "vegetarian", "no cook"]
            ingredients: [
                item("Rolled oats", 50.0, Measure::gram, Aisle::pantry),
                item("Milk", 120.0, Measure::millilitre, Aisle::dairy),
                item("Greek yoghurt", 2.0, Measure::spoon, Aisle::dairy),
                item("Honey", 1.0, Measure::spoon, Aisle::pantry),
                item("Berries", 80.0, Measure::gram, Aisle::frozen),
            ]
            steps: [
                step("Stir the oats, milk, yoghurt and honey together in a jar."),
                step("Close the jar and leave it in the fridge overnight."),
                step("Top with the berries and eat cold."),
            ]
        },
        Recipe {
            id: 3
            title: "Buttermilk pancakes"
            summary: "Tall, soft pancakes for a slow weekend morning."
            course: Course::breakfast
            minutes: 25
            serves: 4
            picture: asset("dishes/pancakes.svg")
            tags: ["weekend", "vegetarian", "sweet"]
            ingredients: [
                item("Plain flour", 200.0, Measure::gram, Aisle::pantry),
                item("Baking powder", 2.0, Measure::spoon, Aisle::pantry),
                item("Sugar", 1.0, Measure::spoon, Aisle::pantry),
                item("Buttermilk", 300.0, Measure::millilitre, Aisle::dairy),
                item("Eggs", 2.0, Measure::piece, Aisle::dairy),
                item("Butter", 30.0, Measure::gram, Aisle::dairy),
                item("Maple syrup", 4.0, Measure::spoon, Aisle::pantry),
            ]
            steps: [
                step("Whisk the flour, baking powder, sugar and a pinch of salt."),
                step("Beat the buttermilk with the eggs and the melted butter."),
                timed("Fold the wet into the dry until just combined and let it rest.", 5),
                timed("Cook ladlefuls in a buttered pan until bubbles break the surface, then flip.", 12),
                step("Stack and pour over the syrup."),
            ]
        },
        Recipe {
            id: 4
            title: "Lentil soup"
            summary: "Red lentils simmered with carrot and cumin, finished with lemon."
            course: Course::lunch
            minutes: 40
            serves: 4
            picture: asset("dishes/soup.svg")
            tags: ["vegan", "batch", "soup"]
            ingredients: [
                item("Olive oil", 2.0, Measure::spoon, Aisle::pantry),
                item("Onion", 1.0, Measure::piece, Aisle::produce),
                item("Carrot", 2.0, Measure::piece, Aisle::produce),
                item("Garlic clove", 3.0, Measure::piece, Aisle::produce),
                item("Cumin", 2.0, Measure::pinch, Aisle::spices),
                item("Red lentils", 250.0, Measure::gram, Aisle::pantry),
                item("Vegetable stock", 1200.0, Measure::millilitre, Aisle::pantry),
                item("Lemon", 1.0, Measure::piece, Aisle::produce),
            ]
            steps: [
                timed("Soften the onion and carrot in the oil.", 8),
                step("Add the garlic and cumin and stir for a minute."),
                timed("Add the lentils and stock and simmer until the lentils fall apart.", 25),
                step("Blend half of it, stir back, and season with lemon juice."),
            ]
        },
        Recipe {
            id: 5
            title: "Caesar salad"
            summary: "Crisp lettuce, garlic croutons and a sharp anchovy dressing."
            course: Course::lunch
            minutes: 20
            serves: 2
            picture: asset("dishes/salad.svg")
            tags: ["salad", "quick"]
            ingredients: [
                item("Romaine lettuce", 1.0, Measure::piece, Aisle::produce),
                item("Sourdough", 2.0, Measure::piece, Aisle::bakery),
                item("Parmesan", 40.0, Measure::gram, Aisle::dairy),
                item("Anchovy fillets", 4.0, Measure::piece, Aisle::pantry),
                item("Egg yolk", 1.0, Measure::piece, Aisle::dairy),
                item("Lemon", 1.0, Measure::piece, Aisle::produce),
                item("Olive oil", 4.0, Measure::spoon, Aisle::pantry),
            ]
            steps: [
                timed("Tear the bread, toss in oil and bake until golden.", 8),
                step("Mash the anchovies with the yolk and lemon, then whisk in the oil."),
                step("Toss the lettuce in the dressing with half the cheese."),
                step("Scatter over the croutons and the rest of the cheese."),
            ]
        },
        Recipe {
            id: 6
            title: "Halloumi wraps"
            summary: "Griddled halloumi, pickled onion and herbs rolled in warm flatbread."
            course: Course::lunch
            minutes: 15
            serves: 2
            picture: asset("dishes/wrap.svg")
            tags: ["vegetarian", "quick"]
            ingredients: [
                item("Halloumi", 225.0, Measure::gram, Aisle::dairy),
                item("Flatbread", 2.0, Measure::piece, Aisle::bakery),
                item("Red onion", 1.0, Measure::piece, Aisle::produce),
                item("Cucumber", 1.0, Measure::piece, Aisle::produce),
                item("Mint", 1.0, Measure::pinch, Aisle::produce),
                item("Greek yoghurt", 3.0, Measure::spoon, Aisle::dairy),
            ]
            steps: [
                step("Slice the onion thin and cover with vinegar and a pinch of sugar."),
                timed("Griddle the halloumi slices until striped on both sides.", 4),
                step("Spread the bread with yoghurt, add cheese, cucumber, onion and mint, and roll."),
            ]
        },
        Recipe {
            id: 7
            title: "Roast chicken"
            summary: "A whole bird over lemon and garlic, with its juices for gravy."
            course: Course::dinner
            minutes: 95
            serves: 4
            picture: asset("dishes/chicken.svg")
            tags: ["sunday", "roast"]
            ingredients: [
                item("Whole chicken", 1.0, Measure::piece, Aisle::produce),
                item("Lemon", 1.0, Measure::piece, Aisle::produce),
                item("Garlic bulb", 1.0, Measure::piece, Aisle::produce),
                item("Thyme", 2.0, Measure::pinch, Aisle::spices),
                item("Butter", 40.0, Measure::gram, Aisle::dairy),
                item("Potatoes", 1000.0, Measure::gram, Aisle::produce),
            ]
            steps: [
                step("Heat the oven to 200°C and let the chicken come to room temperature."),
                step("Rub the bird with butter, salt and thyme; stuff it with the lemon and garlic."),
                timed("Roast on the potatoes, basting halfway.", 80),
                timed("Rest under foil before carving.", 15),
            ]
        },
        Recipe {
            id: 8
            title: "Mushroom risotto"
            summary: "Arborio rice stirred slowly with stock, mushrooms and parmesan."
            course: Course::dinner
            minutes: 45
            serves: 2
            picture: asset("dishes/risotto.svg")
            tags: ["vegetarian", "comfort"]
            ingredients: [
                item("Arborio rice", 180.0, Measure::gram, Aisle::pantry),
                item("Chestnut mushrooms", 250.0, Measure::gram, Aisle::produce),
                item("Shallot", 2.0, Measure::piece, Aisle::produce),
                item("White wine", 100.0, Measure::millilitre, Aisle::pantry),
                item("Vegetable stock", 900.0, Measure::millilitre, Aisle::pantry),
                item("Parmesan", 50.0, Measure::gram, Aisle::dairy),
                item("Butter", 30.0, Measure::gram, Aisle::dairy),
            ]
            steps: [
                timed("Fry the mushrooms hard in half the butter and set aside.", 6),
                timed("Soften the shallots, add the rice and toast it.", 4),
                step("Pour in the wine and let it bubble away."),
                timed("Add the hot stock a ladle at a time, stirring, until creamy.", 20),
                step("Beat in the rest of the butter, the cheese and the mushrooms; rest a minute."),
            ]
        },
        Recipe {
            id: 9
            title: "Thai green curry"
            summary: "Coconut curry with green beans and basil over jasmine rice."
            course: Course::dinner
            minutes: 35
            serves: 4
            picture: asset("dishes/curry.svg")
            tags: ["spicy", "weeknight"]
            ingredients: [
                item("Green curry paste", 3.0, Measure::spoon, Aisle::pantry),
                item("Coconut milk", 400.0, Measure::millilitre, Aisle::pantry),
                item("Chicken thighs", 500.0, Measure::gram, Aisle::produce),
                item("Green beans", 200.0, Measure::gram, Aisle::produce),
                item("Fish sauce", 2.0, Measure::spoon, Aisle::pantry),
                item("Thai basil", 1.0, Measure::pinch, Aisle::produce),
                item("Jasmine rice", 300.0, Measure::gram, Aisle::pantry),
            ]
            steps: [
                timed("Rinse the rice and cook it covered.", 15),
                step("Fry the paste in a spoon of the coconut cream until it splits."),
                timed("Add the chicken and the rest of the coconut milk and simmer.", 12),
                timed("Add the beans and cook until just tender.", 4),
                step("Season with fish sauce, tear in the basil and serve over the rice."),
            ]
        },
        Recipe {
            id: 10
            title: "Spaghetti aglio e olio"
            summary: "Pasta with garlic, chilli and olive oil: dinner from the cupboard."
            course: Course::dinner
            minutes: 15
            serves: 2
            picture: asset("dishes/pasta.svg")
            tags: ["vegan", "quick", "pantry"]
            ingredients: [
                item("Spaghetti", 200.0, Measure::gram, Aisle::pantry),
                item("Garlic clove", 4.0, Measure::piece, Aisle::produce),
                item("Chilli flakes", 1.0, Measure::pinch, Aisle::spices),
                item("Olive oil", 5.0, Measure::spoon, Aisle::pantry),
                item("Parsley", 1.0, Measure::pinch, Aisle::produce),
            ]
            steps: [
                timed("Boil the pasta in well-salted water.", 9),
                step("Warm the sliced garlic and chilli in the oil until the garlic just colours."),
                step("Toss the pasta in the oil with a splash of its water and the parsley."),
            ]
        },
        Recipe {
            id: 11
            title: "Fish tacos"
            summary: "Crisp white fish, lime slaw and chipotle cream in soft tortillas."
            course: Course::dinner
            minutes: 30
            serves: 3
            picture: asset("dishes/tacos.svg")
            tags: ["fish", "weeknight"]
            ingredients: [
                item("White fish fillets", 400.0, Measure::gram, Aisle::frozen),
                item("Corn tortillas", 9.0, Measure::piece, Aisle::bakery),
                item("Red cabbage", 1.0, Measure::piece, Aisle::produce),
                item("Lime", 2.0, Measure::piece, Aisle::produce),
                item("Sour cream", 4.0, Measure::spoon, Aisle::dairy),
                item("Chipotle paste", 1.0, Measure::spoon, Aisle::pantry),
            ]
            steps: [
                step("Shred the cabbage and dress it with lime and salt."),
                step("Stir the chipotle into the sour cream."),
                timed("Dust the fish in seasoned flour and fry until crisp.", 6),
                step("Fill warm tortillas with fish, slaw and the cream."),
            ]
        },
        Recipe {
            id: 12
            title: "Chocolate mousse"
            summary: "Dark chocolate folded into whipped whites, set cool and airy."
            course: Course::dessert
            minutes: 20
            serves: 4
            picture: asset("dishes/mousse.svg")
            tags: ["chocolate", "make ahead"]
            ingredients: [
                item("Dark chocolate", 150.0, Measure::gram, Aisle::pantry),
                item("Eggs", 3.0, Measure::piece, Aisle::dairy),
                item("Sugar", 2.0, Measure::spoon, Aisle::pantry),
                item("Double cream", 100.0, Measure::millilitre, Aisle::dairy),
            ]
            steps: [
                step("Melt the chocolate and let it cool a little."),
                step("Beat the yolks into the chocolate, then fold in the whipped cream."),
                step("Whisk the whites with the sugar to soft peaks and fold in gently."),
                timed("Spoon into glasses and chill until set.", 120),
            ]
        },
        Recipe {
            id: 13
            title: "Apple crumble"
            summary: "Soft spiced apples under a buttery oat crumble."
            course: Course::dessert
            minutes: 50
            serves: 6
            picture: asset("dishes/crumble.svg")
            tags: ["baking", "autumn"]
            ingredients: [
                item("Apples", 6.0, Measure::piece, Aisle::produce),
                item("Cinnamon", 2.0, Measure::pinch, Aisle::spices),
                item("Plain flour", 150.0, Measure::gram, Aisle::pantry),
                item("Rolled oats", 50.0, Measure::gram, Aisle::pantry),
                item("Butter", 120.0, Measure::gram, Aisle::dairy),
                item("Brown sugar", 100.0, Measure::gram, Aisle::pantry),
            ]
            steps: [
                step("Heat the oven to 190°C."),
                timed("Cook the sliced apples with the cinnamon until just soft.", 8),
                step("Rub the butter into the flour, then stir in the oats and sugar."),
                timed("Scatter the crumble over the apples and bake until golden.", 35),
            ]
        },
        Recipe {
            id: 14
            title: "Lemon posset"
            summary: "Cream set with lemon juice alone, sharp and silky."
            course: Course::dessert
            minutes: 15
            serves: 4
            picture: asset("dishes/posset.svg")
            tags: ["make ahead", "citrus"]
            ingredients: [
                item("Double cream", 600.0, Measure::millilitre, Aisle::dairy),
                item("Sugar", 150.0, Measure::gram, Aisle::pantry),
                item("Lemon", 3.0, Measure::piece, Aisle::produce),
            ]
            steps: [
                timed("Boil the cream with the sugar, stirring.", 3),
                step("Take it off the heat and stir in the lemon juice."),
                timed("Pour into glasses and chill until set.", 180),
            ]
        },
        Recipe {
            id: 15
            title: "Hummus"
            summary: "Chickpeas blended smooth with tahini, lemon and garlic."
            course: Course::snack
            minutes: 10
            serves: 6
            picture: asset("dishes/hummus.svg")
            tags: ["vegan", "dip", "no cook"]
            ingredients: [
                item("Chickpeas", 400.0, Measure::gram, Aisle::pantry),
                item("Tahini", 3.0, Measure::spoon, Aisle::pantry),
                item("Lemon", 1.0, Measure::piece, Aisle::produce),
                item("Garlic clove", 1.0, Measure::piece, Aisle::produce),
                item("Olive oil", 3.0, Measure::spoon, Aisle::pantry),
            ]
            steps: [
                step("Blend the tahini and lemon juice until pale."),
                step("Add the chickpeas, garlic and a splash of their water, and blend smooth."),
                step("Spread in a bowl and pour over the oil."),
            ]
        },
        Recipe {
            id: 16
            title: "Spiced nuts"
            summary: "Mixed nuts roasted with maple, rosemary and a little heat."
            course: Course::snack
            minutes: 20
            serves: 6
            picture: asset("dishes/nuts.svg")
            tags: ["gift", "vegan"]
            ingredients: [
                item("Mixed nuts", 300.0, Measure::gram, Aisle::pantry),
                item("Maple syrup", 2.0, Measure::spoon, Aisle::pantry),
                item("Rosemary", 1.0, Measure::pinch, Aisle::spices),
                item("Cayenne", 1.0, Measure::pinch, Aisle::spices),
            ]
            steps: [
                step("Heat the oven to 180°C."),
                step("Toss the nuts with the syrup, rosemary, cayenne and salt."),
                timed("Roast on a lined tray, stirring once, until toasted.", 12),
            ]
        },
    ]
}

export fn starter_shopping() -> List<ShoppingItem> {
    [
        ShoppingItem { id: 1, name: "Milk", aisle: Aisle::dairy },
        ShoppingItem { id: 2, name: "Bananas", aisle: Aisle::produce, note: "ripe" },
        ShoppingItem { id: 3, name: "Sourdough", aisle: Aisle::bakery },
        ShoppingItem { id: 4, name: "Coffee", aisle: Aisle::pantry, bought: true },
    ]
}

export fn starter_plan() -> List<PlannedMeal> {
    [
        PlannedMeal { day: Weekday::monday, course: Course::dinner, recipe: 10 },
        PlannedMeal { day: Weekday::wednesday, course: Course::dinner, recipe: 9 },
        PlannedMeal { day: Weekday::saturday, course: Course::breakfast, recipe: 3 },
        PlannedMeal { day: Weekday::sunday, course: Course::dinner, recipe: 7 },
    ]
}
