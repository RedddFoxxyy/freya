use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Ident,
    Type,
};

#[derive(Debug)]
pub enum QuerySegment {
    Single(FullQuerySegment),
    Segments(Vec<QueryArgument>),
}

impl QuerySegment {
    pub fn contains_ident(&self, ident: &Ident) -> bool {
        match self {
            QuerySegment::Single(segment) => segment.ident == *ident,
            QuerySegment::Segments(segments) => {
                segments.iter().any(|segment| segment.ident == *ident)
            }
        }
    }

    pub fn parse(&self) -> TokenStream2 {
        match self {
            QuerySegment::Single(segment) => segment.parse(),
            QuerySegment::Segments(segments) => {
                let mut tokens = TokenStream2::new();
                tokens.extend(quote! { let split_query: std::collections::HashMap<&str, &str> = query.split('&').filter_map(|s| s.split_once('=')).collect(); });
                for segment in segments {
                    tokens.extend(segment.parse());
                }
                tokens
            }
        }
    }

    pub fn write(&self) -> TokenStream2 {
        match self {
            QuerySegment::Single(segment) => segment.write(),
            QuerySegment::Segments(segments) => {
                let mut tokens = TokenStream2::new();
                tokens.extend(quote! { write!(f, "?")?; });
                let mut segments_iter = segments.iter();
                if let Some(first_segment) = segments_iter.next() {
                    tokens.extend(first_segment.write());
                }
                for segment in segments_iter {
                    tokens.extend(quote! { write!(f, "&")?; });
                    tokens.extend(segment.write());
                }
                tokens
            }
        }
    }

    pub fn parse_from_str<'a>(
        route_span: proc_macro2::Span,
        mut fields: impl Iterator<Item = (&'a Ident, &'a Type)>,
        query: &str,
    ) -> syn::Result<Self> {
        // check if the route has a query string
        if let Some(query) = query.strip_prefix(":..") {
            let query_ident = Ident::new(query, proc_macro2::Span::call_site());
            let field = fields.find(|(name, _)| *name == &query_ident);

            let ty = if let Some((_, ty)) = field {
                ty.clone()
            } else {
                return Err(syn::Error::new(
                    route_span,
                    format!("Could not find a field with the name '{}'", query_ident),
                ));
            };

            Ok(QuerySegment::Single(FullQuerySegment {
                ident: query_ident,
                ty,
            }))
        } else {
            let mut query_arguments = Vec::new();
            for segment in query.split('&') {
                if segment.is_empty() {
                    return Err(syn::Error::new(
                        route_span,
                        "Query segments should be non-empty",
                    ));
                }
                if let Some(query_argument) = segment.strip_prefix(':') {
                    let query_ident = Ident::new(query_argument, proc_macro2::Span::call_site());
                    let field = fields.find(|(name, _)| *name == &query_ident);

                    let ty = if let Some((_, ty)) = field {
                        ty.clone()
                    } else {
                        return Err(syn::Error::new(
                            route_span,
                            format!("Could not find a field with the name '{}'", query_ident),
                        ));
                    };

                    query_arguments.push(QueryArgument {
                        ident: query_ident,
                        ty,
                    });
                } else {
                    return Err(syn::Error::new(
                        route_span,
                        "Query segments should be a : followed by the name of the query argument",
                    ));
                }
            }
            Ok(QuerySegment::Segments(query_arguments))
        }
    }
}

#[derive(Debug)]
pub struct FullQuerySegment {
    pub ident: Ident,
    pub ty: Type,
}

impl FullQuerySegment {
    pub fn parse(&self) -> TokenStream2 {
        let ident = &self.ident;
        let ty = &self.ty;
        quote! {
            let #ident = <#ty as freya_router::routable::FromQuery>::from_query(&*query);
        }
    }

    pub fn write(&self) -> TokenStream2 {
        let ident = &self.ident;
        quote! {
            {
                let as_string = #ident.to_string();
                write!(f, "?{}", freya_router::exports::urlencoding::encode(&as_string))?;
            }
        }
    }
}

#[derive(Debug)]
pub struct QueryArgument {
    pub ident: Ident,
    pub ty: Type,
}

impl QueryArgument {
    pub fn parse(&self) -> TokenStream2 {
        let ident = &self.ident;
        let ty = &self.ty;
        quote! {
            let #ident = match split_query.get(stringify!(#ident)) {
                Some(query_argument) => <#ty as freya_router::routable::FromQueryArgument>::from_query_argument(query_argument).unwrap_or_default(),
                None => <#ty as Default>::default(),
            };
        }
    }

    pub fn write(&self) -> TokenStream2 {
        let ident = &self.ident;
        quote! {
            {
                let as_string = #ident.to_string();
                write!(f, "{}={}", stringify!(#ident), freya_router::exports::urlencoding::encode(&as_string))?;
            }
        }
    }
}
