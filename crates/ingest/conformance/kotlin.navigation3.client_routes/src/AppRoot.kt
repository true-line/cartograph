package com.example

import androidx.compose.runtime.mutableStateListOf
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider

data object HomeDestination : NavKey
data object UsersDestination : NavKey

fun HomeScreen() {}
fun UsersScreen() {}

fun AppRoot() {
    entryProvider {
        entry<HomeDestination> { HomeScreen() }
        entry<UsersDestination> { UsersScreen() }
    }
}

class AppViewModel {
    private val backStack = mutableStateListOf<NavKey>(HomeDestination)

    fun showUsers() {
        backStack.add(UsersDestination)
    }
}
